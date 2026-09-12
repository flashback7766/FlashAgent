//! Adaptive thinking/reasoning profile discovery and management.
//!
//! Different APIs and model backends provide different reasoning effort presets
//! (e.g. `["off", "on"]`, `["off", "low", "medium", "high"]`, `["low", "medium", "high"]`,
//! `["low", "high", "xhigh"]`, or token budgets).
//!
//! Rather than hardcoding static presets, this module discovers what the target API
//! actually advertises (from model metadata or error feedback) and adapts on the fly.

use serde::{Deserialize, Serialize};

/// Wire protocol style used by the API to control thinking/reasoning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThinkingProtocol {
    /// OpenAI-style `reasoning_effort: "<preset>"`
    ReasoningEffort,
    /// OpenRouter/LM Studio object `reasoning: { effort: "<preset>" }`
    ReasoningObject,
    /// Anthropic-style `thinking: { type: "enabled"|"disabled", budget_tokens: N }`
    ThinkingObject,
    /// Boolean flag `enable_thinking: true|false`
    BooleanFlag,
    /// LM Studio native protocol (`reasoning: "<preset>"` and `enable_thinking: bool`)
    LmStudio,
}

/// Discovered model info from the API server (LM Studio, Ollama, OpenAI, OpenRouter...).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredModel {
    /// Identifier used for API calls (e.g. `gemma-4-e2b-it-qat@q4_k_xl`).
    pub id: String,
    /// Human-readable display name if available.
    pub display_name: Option<String>,
    /// Whether this model instance is actively loaded in server memory.
    pub is_loaded: bool,
    /// Loaded or configured context length in tokens.
    pub context_length: Option<usize>,
    /// Maximum context length supported by model architecture in tokens.
    pub max_context_length: Option<usize>,
    /// Reasoning / thinking profile discovered for this model.
    pub thinking: ThinkingProfile,
    /// Whether the model was trained for function / tool use.
    pub supports_tools: bool,
    /// Whether the model supports vision inputs.
    pub supports_vision: bool,
}

impl DiscoveredModel {
    /// Format context length as a human-readable string (e.g. `131k ctx` or `8k ctx`).
    pub fn context_display(&self) -> Option<String> {
        let len = self.context_length.or(self.max_context_length)?;
        if len == 131_072 || len == 128_000 {
            Some("128k ctx".to_string())
        } else if len == 65_536 || len == 64_000 {
            Some("64k ctx".to_string())
        } else if len >= 1024 {
            let k = (len + 512) / 1024;
            Some(format!("{k}k ctx"))
        } else {
            Some(format!("{len} ctx"))
        }
    }

    /// Format a concise summary of capabilities and settings (e.g. `128k ctx · tools · thinking: on [off,on]`).
    pub fn capabilities_summary(&self) -> String {
        let mut parts = Vec::new();

        if let Some(ctx) = self.context_display() {
            parts.push(ctx);
        }

        if self.supports_tools {
            parts.push("tools".to_string());
        }

        if self.supports_vision {
            parts.push("vision".to_string());
        }

        if self.thinking.supported {
            if let Some(ref def) = self.thinking.default_preset {
                if !self.thinking.presets.is_empty() {
                    parts.push(format!("thinking: {} [{}]", def, self.thinking.presets.join(",")));
                } else {
                    parts.push(format!("thinking: {def}"));
                }
            } else if !self.thinking.presets.is_empty() {
                parts.push(format!("thinking: {}", self.thinking.presets.join(",")));
            } else {
                parts.push("thinking".to_string());
            }
        } else {
            parts.push("no reasoning".to_string());
        }

        parts.join(" · ")
    }
}

/// Server discovery result containing all available models and the detected active model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ServerDiscovery {
    /// Base URL of the API endpoint.
    pub base_url: String,
    /// List of models reported by the API.
    pub models: Vec<DiscoveredModel>,
    /// Model currently active or loaded on the server.
    pub active_model: Option<DiscoveredModel>,
}

/// Adaptive thinking profile discovered from the API for a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingProfile {
    /// Presets supported by this model as reported by the API.
    /// E.g. `["off", "on"]`, `["low", "medium", "high"]`, `["off", "low", "medium", "high"]`,
    /// `["low", "high", "xhigh"]`, etc.
    pub presets: Vec<String>,
    /// Protocol used to send the preset to the API.
    pub protocol: ThinkingProtocol,
    /// Whether thinking/reasoning is supported by the API/model at all.
    pub supported: bool,
    /// Default preset advertised by the API server (if known).
    pub default_preset: Option<String>,
}

impl Default for ThinkingProfile {
    fn default() -> Self {
        Self {
            presets: vec![
                "off".to_string(),
                "low".to_string(),
                "medium".to_string(),
                "high".to_string(),
            ],
            protocol: ThinkingProtocol::ReasoningEffort,
            supported: true,
            default_preset: None,
        }
    }
}

/// Estimated complexity of the current turn to dynamically modulate thinking effort.
///
/// Ordered: `Minimal < Low < Medium < High`, so callers can compare levels
/// rather than enumerate them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TaskComplexity {
    /// Trivial greetings, pleasantries, simple confirmations, or minimal queries.
    Minimal,
    /// Routine questions, simple summaries, or basic status responses.
    Low,
    /// Moderately complex questions or balanced analysis.
    Medium,
    /// Code generation, editing files, debugging, analyzing errors/traces, or complex tasks.
    High,
}

impl ThinkingProfile {
    /// Create a profile for an endpoint that does not support thinking parameters.
    pub fn unsupported() -> Self {
        Self {
            presets: Vec::new(),
            protocol: ThinkingProtocol::ReasoningEffort,
            supported: false,
            default_preset: None,
        }
    }

    /// Select the minimum effort level to reduce or disable reasoning (e.g. for Auto-Nudge).
    pub fn min_effort(&self) -> Option<&str> {
        if !self.supported || self.presets.is_empty() {
            return None;
        }
        // First check for explicit disable presets
        for disabled in &["off", "none", "disabled", "false", "0"] {
            if let Some(p) = self.presets.iter().find(|p| p.eq_ignore_ascii_case(disabled)) {
                return Some(p.as_str());
            }
        }
        // If protocol is BooleanFlag or LmStudio, "off" is always a valid effort even if not explicitly in presets
        if self.protocol == ThinkingProtocol::BooleanFlag || self.protocol == ThinkingProtocol::LmStudio {
            return Some("off");
        }
        // Next check for minimal/low presets
        for low in &["low", "minimal", "min", "fast"] {
            if let Some(p) = self.presets.iter().find(|p| p.eq_ignore_ascii_case(low)) {
                return Some(p.as_str());
            }
        }
        // Fall back to the first available preset
        self.presets.first().map(String::as_str)
    }

    /// Select the maximum effort level for deep thinking.
    pub fn max_effort(&self) -> Option<&str> {
        if !self.supported || self.presets.is_empty() {
            return None;
        }
        for high in &["xhigh", "extra-high", "high", "max", "deep", "on"] {
            if let Some(p) = self.presets.iter().find(|p| p.eq_ignore_ascii_case(high)) {
                return Some(p.as_str());
            }
        }
        self.presets.last().map(String::as_str)
    }

    /// Match a requested effort intent against discovered presets.
    pub fn resolve_effort(&self, intent: crate::types::ThinkingEffort) -> Option<&str> {
        match intent {
            crate::types::ThinkingEffort::Auto => {
                // When Auto is resolved without message context, fall back to default or max
                self.default_preset.as_deref().or_else(|| self.max_effort())
            }
            crate::types::ThinkingEffort::Default => self.default_preset.as_deref(),
            crate::types::ThinkingEffort::Off => self.min_effort(),
            crate::types::ThinkingEffort::Low => {
                self.presets
                    .iter()
                    .find(|p| p.eq_ignore_ascii_case("low") || p.eq_ignore_ascii_case("minimal") || p.eq_ignore_ascii_case("min") || p.eq_ignore_ascii_case("fast"))
                    .map(String::as_str)
                    .or_else(|| self.min_effort())
            }
            crate::types::ThinkingEffort::Medium => {
                self.presets
                    .iter()
                    .find(|p| p.eq_ignore_ascii_case("medium") || p.eq_ignore_ascii_case("standard"))
                    .map(String::as_str)
                    .or_else(|| self.presets.get(self.presets.len() / 2).map(String::as_str))
            }
            crate::types::ThinkingEffort::High => self.max_effort(),
        }
    }

    /// Dynamically resolve the optimal thinking preset for the current turn messages.
    pub fn resolve_dynamic(&self, messages: &[crate::types::ChatMessage]) -> Option<&str> {
        if !self.supported || self.presets.is_empty() {
            return None;
        }
        let complexity = Self::analyze_turn_complexity(messages);
        self.resolve_for_complexity(complexity)
    }

    /// Match a task complexity level against discovered model presets.
    pub fn resolve_for_complexity(&self, complexity: TaskComplexity) -> Option<&str> {
        if !self.supported || self.presets.is_empty() {
            return None;
        }
        match complexity {
            TaskComplexity::Minimal => self.min_effort().or(Some("off")),
            TaskComplexity::Low => {
                self.presets
                    .iter()
                    .find(|p| p.eq_ignore_ascii_case("low") || p.eq_ignore_ascii_case("minimal") || p.eq_ignore_ascii_case("min") || p.eq_ignore_ascii_case("fast"))
                    .map(String::as_str)
                    .or_else(|| {
                        // For models with binary on/off presets (e.g. Qwen/DeepSeek in LM Studio), keep "on"
                        // rather than flipping enable_thinking to false and evicting KV prefix cache!
                        self.presets.iter().find(|p| p.eq_ignore_ascii_case("on")).map(String::as_str)
                    })
                    .or_else(|| self.min_effort())
            }
            TaskComplexity::Medium => {
                self.presets
                    .iter()
                    .find(|p| p.eq_ignore_ascii_case("medium") || p.eq_ignore_ascii_case("standard"))
                    .map(String::as_str)
                    .or_else(|| {
                        if self.presets.len() <= 2 {
                            self.max_effort()
                        } else {
                            self.presets.get(self.presets.len() / 2).map(String::as_str)
                        }
                    })
                    .or(self.default_preset.as_deref())
            }
            TaskComplexity::High => {
                self.max_effort().or(self.default_preset.as_deref())
            }
        }
    }

    /// Format payload fields according to the discovered thinking protocol.
    pub fn apply_to_request(&self, body: &mut serde_json::Value, effort: &str) {
        let is_off = effort == "off" || effort == "disabled" || effort == "none" || effort == "false" || effort == "0";
        match self.protocol {
            ThinkingProtocol::ReasoningEffort => {
                if is_off {
                    let off_val = if self.presets.iter().any(|p| p.eq_ignore_ascii_case("off")) {
                        "off"
                    } else {
                        "none"
                    };
                    body["reasoning_effort"] = serde_json::json!(off_val);
                } else {
                    body["reasoning_effort"] = serde_json::json!(effort);
                }
            }
            ThinkingProtocol::ReasoningObject => {
                body["reasoning"] = serde_json::json!({ "effort": effort });
            }
            ThinkingProtocol::ThinkingObject => {
                if is_off {
                    body["thinking"] = serde_json::json!({ "type": "disabled" });
                } else {
                    body["thinking"] = serde_json::json!({ "type": "enabled", "budget_tokens": 1024 });
                }
            }
            ThinkingProtocol::BooleanFlag => {
                let enabled = !is_off;
                body["enable_thinking"] = serde_json::json!(enabled);
                body["chat_template_kwargs"] = serde_json::json!({ "enable_thinking": enabled });
                body["chat_template_config"] = serde_json::json!({ "enable_thinking": enabled });
            }
            ThinkingProtocol::LmStudio => {
                let is_binary_on_off = self.presets.is_empty()
                    || self.presets.iter().all(|p| {
                        let lower = p.to_ascii_lowercase();
                        lower == "on" || lower == "off" || lower == "none" || lower == "disabled"
                    });

                if is_off {
                    body["reasoning"] = serde_json::json!("off");
                    body["enable_thinking"] = serde_json::json!(false);
                    body["chat_template_kwargs"] = serde_json::json!({ "thinking": false, "enable_thinking": false });
                    body["chat_template_config"] = serde_json::json!({ "thinking": false, "enable_thinking": false });
                    if !is_binary_on_off {
                        let off_val = if self.presets.iter().any(|p| p.eq_ignore_ascii_case("off")) {
                            "off"
                        } else {
                            "none"
                        };
                        body["reasoning_effort"] = serde_json::json!(off_val);
                    }
                } else {
                    body["enable_thinking"] = serde_json::json!(true);
                    body["chat_template_kwargs"] = serde_json::json!({ "enable_thinking": true });
                    body["chat_template_config"] = serde_json::json!({ "enable_thinking": true });
                    if is_binary_on_off {
                        body["reasoning"] = serde_json::json!("on");
                    } else {
                        let matched = self
                            .presets
                            .iter()
                            .find(|p| p.eq_ignore_ascii_case(effort))
                            .map(|s| s.as_str())
                            .or_else(|| match effort {
                                "on" | "auto" => self
                                    .presets
                                    .iter()
                                    .find(|p| p.eq_ignore_ascii_case("medium"))
                                    .map(|s| s.as_str()),
                                _ => None,
                            })
                            .unwrap_or(effort);
                        body["reasoning"] = serde_json::json!(matched);
                        body["reasoning_effort"] = serde_json::json!(matched);
                    }
                }
            }
        }
    }

/// How hard this turn looks, and why.
///
/// The rule that matters: the level is decided by **the user's own message**
/// and then held for every step of that task. A turn that reads three files
/// and runs a test is one task, not four decisions — flipping the reasoning
/// preset between steps costs the backend its prefix cache and tells the
/// model nothing new. A failed tool result is the one thing that raises it.
///
/// Signals are structural rather than lexical. The old version matched a list
/// of English words, so a Russian prompt matched nothing, and it measured
/// length in bytes, so any Cyrillic sentence over fifty characters counted as
/// "long" and went to maximum reasoning. Both of those are how a model ends
/// up thinking for forty seconds about "прочитай файл".
pub fn analyze_turn_complexity(messages: &[crate::types::ChatMessage]) -> TaskComplexity {
    use crate::types::Role;

    let Some(last) = messages.last() else {
        return TaskComplexity::Medium;
    };

    // Something the model tried has failed: that is worth thinking about,
    // whatever the task looked like when it started.
    let last_failed = last.role == Role::Tool && Self::looks_like_failure(&last.content);

    // The task's own message, not whatever the last tool printed — and not
    // the "yes" that approved a step either: answering a question mid-task
    // does not make the task trivial.
    let mut base = messages
        .iter()
        .rev()
        .filter(|m| m.role == Role::User)
        .find(|m| !Self::is_bare_acknowledgement(Self::user_text(&m.content)))
        .map(|m| Self::user_message_complexity(&m.content))
        .unwrap_or(TaskComplexity::Minimal);

    // "делай" / "yes, go ahead" approves whatever was just proposed, and the
    // proposal is the task. Without this, agreeing to a full refactor scores
    // like the word "делай".
    let last_user_is_ack = messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .is_some_and(|m| Self::is_bare_acknowledgement(Self::user_text(&m.content)));
    if last_user_is_ack {
        if let Some(plan) = messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant && !m.content.trim().is_empty())
        {
            base = base.max(Self::user_message_complexity(&plan.content));
        }
    }

    if last_failed {
        return Self::raise(base);
    }
    base
}

/// One step up, never past the top.
fn raise(level: TaskComplexity) -> TaskComplexity {
    match level {
        TaskComplexity::Minimal => TaskComplexity::Low,
        TaskComplexity::Low => TaskComplexity::Medium,
        TaskComplexity::Medium | TaskComplexity::High => TaskComplexity::High,
    }
}

/// Whether a tool result reads as a failure, in any language: the markers are
/// program output, not prose.
fn looks_like_failure(content: &str) -> bool {
    let c = content.trim();
    let lower = c.to_lowercase();
    lower.starts_with("error")
        || lower.starts_with("failed")
        || c.contains("Traceback (most recent call last)")
        || c.contains("panicked at")
        || lower.contains("build failed")
        || lower.contains("test failed")
        || lower.contains("compilation failed")
        || lower.contains("permission denied")
        || lower.contains("no such file")
        // "exit code: 0" is a success; anything else is not.
        || (lower.contains("exit code:") && !lower.contains("exit code: 0"))
}

/// Explicit instructions about reasoning, in the two languages the app is
/// used in. These are the user overriding the guess, so they win outright.
fn explicit_override(lower: &str) -> Option<TaskComplexity> {
    const BRIEF: &[&str] = &[
        "no thinking", "don't think", "do not think", "without thinking", "no reasoning",
        "in one line", "one word", "short answer", "briefly", "be brief", "concise",
        "без размышлен", "не думай", "коротко", "кратко", "в одну строку", "одним словом",
    ];
    const DEEP: &[&str] = &[
        "think step by step", "think deeply", "reason carefully", "deep reasoning",
        "thoroughly", "in detail", "full analysis",
        "подумай", "тщательно", "подробно", "детально", "разберись",
    ];
    if BRIEF.iter().any(|k| lower.contains(k)) {
        return Some(TaskComplexity::Minimal);
    }
    if DEEP.iter().any(|k| lower.contains(k)) {
        return Some(TaskComplexity::High);
    }
    None
}

/// The user's own words, without the memory block we inject ahead of them.
fn user_text(raw: &str) -> &str {
    match raw.rsplit_once("\n\n---\n\n") {
        Some((_memory, user_part)) => user_part.trim(),
        None => raw.trim(),
    }
}

/// "yes", "1", "ок", "давай" — an answer to the agent, not a task.
fn is_bare_acknowledgement(prompt: &str) -> bool {
    prompt.chars().count() <= 12
        && prompt.split_whitespace().count() <= 2
        && !prompt.contains('?')
}

/// Score one user message by what it is shaped like.
fn user_message_complexity(raw: &str) -> TaskComplexity {
    let prompt = Self::user_text(raw);
    let lower = prompt.to_lowercase();

    if let Some(explicit) = Self::explicit_override(&lower) {
        return explicit;
    }
    if crate::thinking::is_greeting_text(prompt) {
        return TaskComplexity::Minimal;
    }

    // Characters, not bytes: a Cyrillic sentence is not twice as hard as the
    // same sentence in English.
    let chars = prompt.chars().count();

    if Self::is_bare_acknowledgement(prompt) {
        return TaskComplexity::Minimal;
    }

    let mut score = 0i32;

    // Several things asked for at once, or a sequence to carry out.
    let multi_step = prompt.lines().filter(|l| {
        let t = l.trim_start();
        t.starts_with("- ") || t.starts_with("* ") || t.starts_with(|c: char| c.is_ascii_digit())
    }).count() >= 2
        || lower.contains(" then ")
        || lower.contains(", then")
        || lower.contains("потом")
        || lower.contains("затем")
        || lower.contains("после чего")
        || lower.contains(" and then ");
    if multi_step {
        score += 2;
    }

    // Code the user pasted, or symbols they are pointing at.
    if prompt.contains("```") {
        score += 2;
    }
    let code_marks = ["::", "->", "=>", "fn ", "def ", "class ", "impl ", "struct ", "()", "{}", "<T>"];
    if code_marks.iter().filter(|m| prompt.contains(**m)).count() >= 2 {
        score += 2;
    }

    // A path or a file: concrete work on something that exists.
    let has_path = prompt.split_whitespace().any(|w| {
        let w = w.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '/' && c != '_');
        (w.contains('/') && !w.contains("://")) || w.rsplit_once('.').is_some_and(|(stem, ext)| {
            !stem.is_empty() && (2..=4).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphabetic())
        })
    });
    if has_path {
        score += 1;
    }

    // Something is broken, in either language.
    const TROUBLE: &[&str] = &[
        "error", "panic", "crash", "fails", "failing", "broken", "bug", "regress", "why does",
        "ошибк", "падает", "ломает", "не работает", "баг", "почему",
    ];
    if TROUBLE.iter().any(|k| lower.contains(k)) {
        score += 2;
    }

    // Asking for something to be produced or changed is work; asking a
    // question about it is not. This is the one lexical signal kept, it is
    // short, and it is written in both languages the app is used in — the
    // list it replaced had forty English words including "run" and "check",
    // which fire on nearly every sentence a programmer types.
    const MAKE: &[&str] = &[
        "write ", "create ", "implement ", "add ", "fix ", "refactor", "rename", "delete ",
        "remove ", "update ", "migrate", "port ", "optimis", "optimiz",
        "напиш", "создай", "добавь", "исправь", "почини", "переимен", "удали", "обнови",
        "рефактор", "реализуй", "перепиш",
    ];
    if MAKE.iter().any(|k| lower.contains(k)) {
        score += 1;
    }

    // Scope. "Refactor the parser" and "refactor the whole project" are the
    // same length and the same verb; only one of them is a week of work.
    const WHOLE: &[&str] = &[
        "whole project", "entire project", "whole codebase", "entire codebase", "all files",
        "everywhere", "across the codebase", "the whole thing", "from scratch", "rewrite everything",
        "весь проект", "всего проекта", "всему проекту", "полный", "полностью", "везде",
        "во всех файлах", "с нуля", "всё приложение", "все файлы",
    ];
    if WHOLE.iter().any(|k| lower.contains(k)) {
        score += 2;
    }

    // Length, in characters.
    if chars > 240 {
        score += 2;
    } else if chars > 80 {
        score += 1;
    }

    // A question at all.
    if prompt.contains('?') {
        score += 1;
    }

    match score {
        // Nothing in it suggests work at all. Short and empty-handed is small
        // talk; long and empty-handed is still a question worth answering.
        0 if chars <= 40 => TaskComplexity::Minimal,
        0 => TaskComplexity::Low,
        1..=2 => TaskComplexity::Medium,
        _ => TaskComplexity::High,
    }
}


    /// Parse model metadata from `/v1/models` or `/v1/models/{model}` response JSON.
    pub fn parse_model_metadata(data: &serde_json::Value, model_name: &str) -> Option<Self> {
        let models = parse_server_models(data);
        for m in models {
            if (m.id == model_name || m.id.contains(model_name) || model_name.contains(&m.id))
                && m.thinking.supported
            {
                return Some(m.thinking);
            }
        }
        None
    }

    /// Parse error responses from the API to learn supported presets on the fly.
    pub fn parse_api_error(error_body: &str) -> Option<Self> {
        let err = error_body.to_lowercase();
        // Only an error about the thinking controls can teach presets; any
        // other 400 ("invalid messages[3].content") must not be mined for
        // bracketed lists.
        if !["reasoning", "thinking", "effort", "supported settings"].iter().any(|k| err.contains(k)) {
            return None;
        }

        // Check if reasoning_effort / thinking is completely rejected by the server
        if err.contains("unrecognized request argument: reasoning_effort")
            || err.contains("unknown parameter: reasoning_effort")
            || (err.contains("extra inputs are not permitted") && err.contains("reasoning"))
            || err.contains("unsupported parameter: reasoning")
        {
            return Some(Self::unsupported());
        }

        // Look for preset list in brackets: e.g. `['low', 'medium', 'high']` or `[off, on]`
        if let Some(start_bracket) = err.find('[') {
            if let Some(end_bracket) = err[start_bracket..].find(']') {
                let inside = &err[start_bracket + 1..start_bracket + end_bracket];
                let presets: Vec<String> = inside
                    .split(',')
                    .map(|s| s.trim().trim_matches('\'').trim_matches('"').trim().to_string())
                    .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                    .collect();

                if !presets.is_empty() {
                    let protocol = if err.contains("supported settings") || (presets.contains(&"off".to_string()) && presets.contains(&"on".to_string())) {
                        ThinkingProtocol::LmStudio
                    } else if err.contains("reasoning_effort") {
                        ThinkingProtocol::ReasoningEffort
                    } else if err.contains("reasoning") {
                        ThinkingProtocol::ReasoningObject
                    } else {
                        ThinkingProtocol::ReasoningEffort
                    };

                    return Some(Self {
                        presets,
                        protocol,
                        supported: true,
                        default_preset: None,
                    });
                }
            }
        }

        // Look for comma-separated list following `one of:`, `expected:`, or `supported settings:`
        for prefix in &["one of:", "one of :", "expected:", "supported settings:", "supported values:"] {
            if let Some(idx) = err.find(prefix) {
                let after = err[idx + prefix.len()..].trim();
                let chunk = after.split('.').next().unwrap_or(after);
                let presets: Vec<String> = chunk
                    .split([',', '/', '|'])
                    .map(|s| s.trim().trim_matches('\'').trim_matches('"').trim().to_string())
                    .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                    .collect();

                if presets.len() >= 2 {
                    let protocol = if presets.contains(&"off".to_string()) && presets.contains(&"on".to_string()) {
                        ThinkingProtocol::LmStudio
                    } else {
                        ThinkingProtocol::ReasoningEffort
                    };
                    return Some(Self {
                        presets,
                        protocol,
                        supported: true,
                        default_preset: None,
                    });
                }
            }
        }

        None
    }
}

/// Returns true if the prompt is a casual greeting, pleasantry, or introductory pleasantry
/// without code syntax, technical commands, or complex task instructions.
pub fn is_greeting_text(prompt: &str) -> bool {
    let raw = prompt.trim();
    if raw.is_empty() {
        return false;
    }

    let clean_lower = raw
        .trim_matches(|c: char| !c.is_alphanumeric() && !c.is_whitespace())
        .to_lowercase();
    if clean_lower.is_empty() {
        return false;
    }

    // Direct match against known single/multi-word pleasantries
    let is_exact = matches!(
        clean_lower.as_str(),
        "hi" | "hello" | "hey" | "howdy" | "sup" | "yo" | "bye" | "goodbye" | "cya" | "see ya"
        | "thanks" | "thank you" | "thx" | "cool" | "nice" | "great" | "awesome" | "cheers"
        | "help" | "who are you" | "how are you" | "what can you do" | "what are you doing" | "what's up" | "whats up"
        | "good morning" | "good afternoon" | "good evening" | "good night"
    );
    if is_exact {
        return true;
    }

    // Word tokenization (splits on any non-alphanumeric character)
    let words: Vec<&str> = raw
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect();

    if words.is_empty() {
        return false;
    }

    let first = words[0].to_lowercase();
    let is_greeting_head = matches!(
        first.as_str(),
        "hello" | "hi" | "hey" | "howdy" | "sup" | "yo"
    );

    let is_greeting_phrase = first == "good";

    if is_greeting_head || is_greeting_phrase {
        // Single word greeting like "Hi!"
        if words.len() == 1 {
            return true;
        }
        // Conversational greeting if all subsequent words are pleasantry words
        let is_all_pleasantry = words[1..].iter().all(|w| {
            let wl = w.to_lowercase();
            matches!(
                wl.as_str(),
                "there" | "all" | "everyone" | "friend" | "bro" | "agent" | "today"
                | "how" | "are" | "you" | "is" | "it" | "going" | "up" | "doing"
                | "what" | "new"
                | "morning" | "afternoon" | "evening" | "night" | "day"
            )
        });
        if is_all_pleasantry && words.len() <= 6 {
            return true;
        }
    }

    false
}

/// Analyze the turn messages to classify task complexity for dynamic reasoning.
pub fn analyze_turn_complexity(messages: &[crate::types::ChatMessage]) -> TaskComplexity {
    ThinkingProfile::analyze_turn_complexity(messages)
}

/// Parse models and capabilities from server JSON (LM Studio `/api/v1/models`,
/// `/api/v0/models`, or OpenAI-compatible `/v1/models`).
pub fn parse_server_models(data: &serde_json::Value) -> Vec<DiscoveredModel> {
    let mut discovered = Vec::new();

    // Check 1: LM Studio native v1 API `{"models": [...]}`
    if let Some(models) = data.get("models").and_then(|m| m.as_array()) {
        for m in models {
            let id = m.get("key")
                .or_else(|| m.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            if id.is_empty() {
                continue;
            }

            let display_name = m.get("display_name").and_then(|v| v.as_str()).map(String::from);
            let loaded_instances = m.get("loaded_instances").and_then(|v| v.as_array());
            let is_loaded = loaded_instances.map(|arr| !arr.is_empty()).unwrap_or(false);

            let context_length = loaded_instances
                .and_then(|arr| arr.first())
                .and_then(|inst| inst.get("config"))
                .and_then(|cfg| cfg.get("context_length"))
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);

            let max_context_length = m.get("max_context_length")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);

            let mut supports_tools = false;
            let mut supports_vision = false;
            let mut thinking = ThinkingProfile::unsupported();

            if let Some(caps) = m.get("capabilities") {
                supports_tools = caps.get("trained_for_tool_use").and_then(|v| v.as_bool()).unwrap_or(false);
                supports_vision = caps.get("vision").and_then(|v| v.as_bool()).unwrap_or(false);

                if let Some(reasoning) = caps.get("reasoning") {
                    let mut presets = Vec::new();
                    if let Some(opts) = reasoning.get("allowed_options").and_then(|v| v.as_array()) {
                        for opt in opts {
                            if let Some(s) = opt.as_str() {
                                presets.push(s.to_lowercase());
                            }
                        }
                    }
                    let default_preset = reasoning.get("default").and_then(|v| v.as_str()).map(|s| s.to_lowercase());
                    if !presets.is_empty() {
                        thinking = ThinkingProfile {
                            presets,
                            protocol: ThinkingProtocol::LmStudio,
                            supported: true,
                            default_preset,
                        };
                    }
                }
            }

            // Fallback for chat_template_config enable_thinking
            if !thinking.supported {
                if let Some(cfg) = m.get("chat_template_config") {
                    if cfg.get("enable_thinking").is_some() {
                        thinking = ThinkingProfile {
                            presets: vec!["off".to_string(), "on".to_string()],
                            protocol: ThinkingProtocol::LmStudio,
                            supported: true,
                            default_preset: Some("on".to_string()),
                        };
                    }
                }
            }

            if !thinking.supported {
                let id_lower = id.to_lowercase();
                let arch_lower = m.get("architecture").or_else(|| m.get("arch")).and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
                if id_lower.contains("qwen")
                    || id_lower.contains("deepseek")
                    || id_lower.contains("think")
                    || id_lower.contains("reason")
                    || id_lower.contains("gemma4")
                    || id_lower.contains("gemma-4")
                    || arch_lower.contains("qwen")
                    || arch_lower.contains("deepseek")
                    || arch_lower.contains("gemma4")
                {
                    thinking = ThinkingProfile {
                        presets: vec!["off".to_string(), "on".to_string()],
                        protocol: ThinkingProtocol::LmStudio,
                        supported: true,
                        default_preset: Some("on".to_string()),
                    };
                }
            }

            discovered.push(DiscoveredModel {
                id: id.to_string(),
                display_name,
                is_loaded,
                context_length,
                max_context_length,
                thinking,
                supports_tools,
                supports_vision,
            });
        }
        if !discovered.is_empty() {
            return discovered;
        }
    }

    // Check 2: LM Studio v0 or OpenAI list `{"data": [...]}` or bare array `[...]`
    let items = if let Some(arr) = data.get("data").and_then(|d| d.as_array()) {
        arr.as_slice()
    } else if let Some(arr) = data.as_array() {
        arr.as_slice()
    } else if data.is_object() {
        std::slice::from_ref(data)
    } else {
        &[]
    };

    for m in items {
        let id = m.get("id")
            .or_else(|| m.get("key"))
            .or_else(|| m.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if id.is_empty() {
            continue;
        }

        let is_loaded = m.get("state").and_then(|v| v.as_str()).map(|s| s == "loaded").unwrap_or(false);
        let context_length = m.get("loaded_context_length")
            .or_else(|| m.get("context_length"))
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);
        let max_context_length = m.get("max_context_length")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);

        let mut supports_tools = false;
        let mut thinking = ThinkingProfile::unsupported();

        if let Some(caps) = m.get("capabilities") {
            if let Some(arr) = caps.as_array() {
                supports_tools = arr.iter().any(|v| v.as_str() == Some("tool_use"));
            } else if let Some(obj) = caps.as_object() {
                supports_tools = obj.get("trained_for_tool_use").and_then(|v| v.as_bool()).unwrap_or(false);
                if let Some(levels) = obj.get("reasoning_effort_levels").or_else(|| obj.get("reasoning_efforts")).and_then(|v| v.as_array()) {
                    let presets: Vec<String> = levels.iter().filter_map(|v| v.as_str().map(|s| s.to_lowercase())).collect();
                    if !presets.is_empty() {
                        thinking = ThinkingProfile {
                            presets,
                            protocol: ThinkingProtocol::ReasoningEffort,
                            supported: true,
                            default_preset: None,
                        };
                    }
                }
            }
        }

        if !thinking.supported {
            let id_lower = id.to_lowercase();
            let arch_lower = m.get("architecture").or_else(|| m.get("arch")).and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
            if id_lower.contains("qwen")
                || id_lower.contains("deepseek")
                || id_lower.contains("think")
                || id_lower.contains("reason")
                || id_lower.contains("gemma4")
                || id_lower.contains("gemma-4")
                || arch_lower.contains("qwen")
                || arch_lower.contains("deepseek")
                || arch_lower.contains("gemma4")
            {
                thinking = ThinkingProfile {
                    presets: vec!["off".to_string(), "on".to_string()],
                    protocol: ThinkingProtocol::LmStudio,
                    supported: true,
                    default_preset: Some("on".to_string()),
                };
            }
        }

        discovered.push(DiscoveredModel {
            id: id.to_string(),
            display_name: None,
            is_loaded,
            context_length,
            max_context_length,
            thinking,
            supports_tools,
            supports_vision: false,
        });
    }

    discovered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_min_and_max_effort_selection() {
        // Preset: off/on
        let p_binary = ThinkingProfile {
            presets: vec!["off".into(), "on".into()],
            protocol: ThinkingProtocol::BooleanFlag,
            supported: true,
            default_preset: Some("on".into()),
        };
        assert_eq!(p_binary.min_effort(), Some("off"));
        assert_eq!(p_binary.max_effort(), Some("on"));
        assert_eq!(p_binary.resolve_effort(crate::types::ThinkingEffort::Default), Some("on"));

        // Preset: off/low/medium/high
        let p_four = ThinkingProfile {
            presets: vec!["off".into(), "low".into(), "medium".into(), "high".into()],
            protocol: ThinkingProtocol::ReasoningEffort,
            supported: true,
            default_preset: None,
        };
        assert_eq!(p_four.min_effort(), Some("off"));
        assert_eq!(p_four.max_effort(), Some("high"));

        // Preset: low/medium/high (no off)
        let p_classic = ThinkingProfile {
            presets: vec!["low".into(), "medium".into(), "high".into()],
            protocol: ThinkingProtocol::ReasoningEffort,
            supported: true,
            default_preset: None,
        };
        assert_eq!(p_classic.min_effort(), Some("low"));
        assert_eq!(p_classic.max_effort(), Some("high"));

        // Preset: low/high/xhigh
        let p_xhigh = ThinkingProfile {
            presets: vec!["low".into(), "high".into(), "xhigh".into()],
            protocol: ThinkingProtocol::ReasoningEffort,
            supported: true,
            default_preset: None,
        };
        assert_eq!(p_xhigh.min_effort(), Some("low"));
        assert_eq!(p_xhigh.max_effort(), Some("xhigh"));
    }

    #[test]
    fn test_parse_api_error_extracts_presets() {
        // OpenAI / LM Studio format with brackets
        let err1 = "Invalid reasoning_effort 'off'. Supported values are: ['low', 'medium', 'high']";
        let p1 = ThinkingProfile::parse_api_error(err1).expect("parsed p1");
        assert_eq!(p1.presets, vec!["low", "medium", "high"]);
        assert_eq!(p1.min_effort(), Some("low"));

        // Alternative presets: low/high/xhigh
        let err2 = "reasoning_effort must be one of: ['low', 'high', 'xhigh']";
        let p2 = ThinkingProfile::parse_api_error(err2).expect("parsed p2");
        assert_eq!(p2.presets, vec!["low", "high", "xhigh"]);
        assert_eq!(p2.max_effort(), Some("xhigh"));

        // Binary presets: [off, on]
        let err3 = "Invalid value for reasoning. Expected one of: [off, on]";
        let p3 = ThinkingProfile::parse_api_error(err3).expect("parsed p3");
        assert_eq!(p3.presets, vec!["off", "on"]);
        assert_eq!(p3.min_effort(), Some("off"));

        // Unsupported error
        let err4 = "Unrecognized request argument: reasoning_effort";
        let p4 = ThinkingProfile::parse_api_error(err4).expect("parsed p4");
        assert!(!p4.supported);
        assert_eq!(p4.min_effort(), None);

        // LM Studio warning/error format: "Supported settings: 'on', 'off'"
        let err5 = "Reasoning setting 'high' is not supported by model. Supported settings: 'on', 'off'. Falling back to reasoning setting 'on'.";
        let p5 = ThinkingProfile::parse_api_error(err5).expect("parsed p5");
        assert_eq!(p5.presets, vec!["on", "off"]);
        assert_eq!(p5.protocol, ThinkingProtocol::LmStudio);
    }

    #[test]
    fn test_parse_server_models_lm_studio_v1() {
        let json = serde_json::json!({
            "models": [
                {
                    "type": "llm",
                    "publisher": "unsloth",
                    "key": "gemma-4-e2b-it-qat@q4_k_xl",
                    "display_name": "Gemma 4 E2B Instruct QAT UD",
                    "architecture": "gemma4",
                    "loaded_instances": [
                        {
                            "id": "gemma-4-e2b-it-qat@q4_k_xl",
                            "config": {
                                "context_length": 131072
                            }
                        }
                    ],
                    "max_context_length": 131072,
                    "capabilities": {
                        "vision": false,
                        "trained_for_tool_use": true,
                        "reasoning": {
                            "allowed_options": [
                                "off",
                                "on"
                            ],
                            "default": "on"
                        }
                    }
                }
            ]
        });

        let models = parse_server_models(&json);
        assert_eq!(models.len(), 1);
        let m = &models[0];
        assert_eq!(m.id, "gemma-4-e2b-it-qat@q4_k_xl");
        assert_eq!(m.display_name.as_deref(), Some("Gemma 4 E2B Instruct QAT UD"));
        assert!(m.is_loaded);
        assert_eq!(m.context_length, Some(131072));
        assert_eq!(m.context_display().as_deref(), Some("128k ctx"));
        let mut m_64k = m.clone();
        m_64k.context_length = Some(65536);
        assert_eq!(m_64k.context_display().as_deref(), Some("64k ctx"));
        m_64k.context_length = Some(64000);
        assert_eq!(m_64k.context_display().as_deref(), Some("64k ctx"));
        let mut m_128k = m.clone();
        m_128k.context_length = Some(128000);
        assert_eq!(m_128k.context_display().as_deref(), Some("128k ctx"));
        assert!(m.supports_tools);
        assert!(!m.supports_vision);
        assert!(m.thinking.supported);
        assert_eq!(m.thinking.presets, vec!["off", "on"]);
        assert_eq!(m.thinking.protocol, ThinkingProtocol::LmStudio);
        assert_eq!(m.thinking.default_preset.as_deref(), Some("on"));

        // Verify parse_model_metadata finds it
        let prof = ThinkingProfile::parse_model_metadata(&json, "gemma-4").expect("found profile");
        assert_eq!(prof.presets, vec!["off", "on"]);
    }

    #[test]
    fn greetings_and_acknowledgements_need_no_reasoning() {
        use crate::types::ChatMessage;
        for text in ["Hello!", "thanks, ok", "What's up bro!", "Good morning", "ок", "да"] {
            assert_eq!(
                analyze_turn_complexity(&[ChatMessage::user(text)]),
                TaskComplexity::Minimal,
                "{text}"
            );
        }
        // The memory block we inject is ours, not the user's message.
        assert_eq!(
            analyze_turn_complexity(&[ChatMessage::user(
                "# Memory (automatically loaded)\n\n---\n\nHello"
            )]),
            TaskComplexity::Minimal
        );
    }

    #[test]
    fn a_russian_prompt_is_judged_like_the_same_prompt_in_english() {
        use crate::types::ChatMessage;
        // The old version measured length in bytes, so this sentence — 57
        // characters, 94 bytes — counted as "long" and went to maximum
        // reasoning purely for being Cyrillic.
        let ru = analyze_turn_complexity(&[ChatMessage::user(
            "Прочитай src/parser.rs и коротко опиши что делает функция",
        )]);
        let en = analyze_turn_complexity(&[ChatMessage::user(
            "Read src/parser.rs and briefly describe what the function does",
        )]);
        assert_eq!(ru, en, "same request, same effort");
        assert!(ru <= TaskComplexity::Medium, "a file read is not maximum-reasoning work: {ru:?}");
    }

    #[test]
    fn ordinary_work_does_not_get_maximum_reasoning() {
        use crate::types::ChatMessage;
        for text in [
            "Write a JSON parsing function in Rust",
            "Добавь док-комментарий над parse_duration",
            "What is DNS?",
            "Rename the cache field to store",
        ] {
            let level = analyze_turn_complexity(&[ChatMessage::user(text)]);
            assert!(
                level <= TaskComplexity::Medium,
                "{text} came out as {level:?}; High is for work that is failing or multi-step"
            );
        }
    }

    #[test]
    fn hard_work_is_recognised_as_hard() {
        use crate::types::ChatMessage;
        for text in [
            "Fix the panic in parser.rs, then add a test for it and run the suite",
            "Почини ошибку в src/loop.rs, потом прогони тесты",
            "Think step by step and compare B-trees and LSM-trees",
            "Why does the build fail with a borrow error in src/main.rs?",
        ] {
            assert_eq!(
                analyze_turn_complexity(&[ChatMessage::user(text)]),
                TaskComplexity::High,
                "{text}"
            );
        }
    }

    #[test]
    fn the_user_can_say_how_much_thinking_they_want() {
        use crate::types::ChatMessage;
        for brief in [
            "Name the capital of France, answer in one line without reasoning",
            "Кратко: что делает эта функция?",
        ] {
            assert_eq!(
                analyze_turn_complexity(&[ChatMessage::user(brief)]),
                TaskComplexity::Minimal,
                "{brief}"
            );
        }
        assert_eq!(
            analyze_turn_complexity(&[ChatMessage::user("Подробно разбери архитектуру цикла")]),
            TaskComplexity::High
        );
    }

    #[test]
    fn agreeing_to_a_big_job_inherits_the_job() {
        use crate::types::ChatMessage;

        // The user states the job, then approves it a message later. "делай"
        // is two syllables; the task behind it is a week.
        let stated = vec![
            ChatMessage::user("Делаем полный рефактор проекта"),
            ChatMessage::assistant("Хорошо, начну с разбора зависимостей. Приступать?"),
            ChatMessage::user("делай"),
        ];
        assert_eq!(analyze_turn_complexity(&stated), TaskComplexity::High);

        // And the other way round: the assistant proposes the big job, the
        // user only says yes, so the proposal is the task.
        let proposed = vec![
            ChatMessage::user("Что тут можно улучшить?"),
            ChatMessage::assistant(
                "Предлагаю полный рефактор проекта: вынести цикл в отдельный модуль, \
                 переписать разбор аргументов и прогнать тесты. Делаем?",
            ),
            ChatMessage::user("да"),
        ];
        assert_eq!(analyze_turn_complexity(&proposed), TaskComplexity::High);
    }

    #[test]
    fn scope_counts_even_when_the_sentence_is_short() {
        use crate::types::ChatMessage;
        let one_file = analyze_turn_complexity(&[ChatMessage::user("Refactor the parser")]);
        let whole = analyze_turn_complexity(&[ChatMessage::user("Refactor the whole project")]);
        assert!(whole > one_file, "{whole:?} vs {one_file:?}");
        assert_eq!(whole, TaskComplexity::High);
    }

    #[test]
    fn a_failed_tool_raises_the_level_but_a_successful_one_does_not() {
        use crate::types::{ChatMessage, Role};
        let tool = |content: &str| ChatMessage {
            role: Role::Tool,
            content: content.to_string(),
            reasoning: None,
            tool_call_id: Some("1".into()),
            tool_calls: vec![],
        };
        let task = ChatMessage::user("Read src/parser.rs and describe it");
        let base = analyze_turn_complexity(std::slice::from_ref(&task));

        // A step that worked keeps the task at its own level: flipping the
        // preset between steps costs the backend its prefix cache.
        assert_eq!(
            analyze_turn_complexity(&[task.clone(), tool("exit code: 0\noutput:\ndone")]),
            base
        );

        // A step that failed is worth more thought than the task asked for.
        for failure in [
            "error: no such file: src/confg.rs",
            "exit code: 101\noutput:\ntest failures",
            "panicked at src/main.rs:42",
        ] {
            assert!(
                analyze_turn_complexity(&[task.clone(), tool(failure)]) > base,
                "{failure}"
            );
        }
    }

    #[test]
    fn the_level_holds_across_the_steps_of_one_task() {
        use crate::types::{ChatMessage, Role};
        // Answering "yes" mid-task is not a new, trivial turn: the task is
        // what is being worked on, and its level is what applies.
        let history = vec![
            ChatMessage::user("Fix the compilation error in src/main.rs, then run the tests"),
            ChatMessage::assistant("Found a type error. Fix it now?"),
            ChatMessage::user("Yes"),
        ];
        assert_eq!(analyze_turn_complexity(&history), TaskComplexity::High);

        let mut deep = history.clone();
        deep.push(ChatMessage {
            role: Role::Tool,
            content: "exit code: 0".into(),
            reasoning: None,
            tool_call_id: Some("2".into()),
            tool_calls: vec![],
        });
        assert_eq!(
            analyze_turn_complexity(&deep),
            TaskComplexity::High,
            "the level must not drop halfway through a task"
        );
    }

    #[test]
    fn test_is_greeting_text_variants() {
        assert!(is_greeting_text("Hello!"));
        assert!(is_greeting_text("Hi there"));
        assert!(is_greeting_text("Hey!"));
        assert!(is_greeting_text("Good morning"));
        assert!(is_greeting_text("Hello there"));
        assert!(is_greeting_text("Hi!"));
        assert!(is_greeting_text("Thanks"));
        assert!(is_greeting_text("Thank you!"));
        assert!(is_greeting_text("Who are you?"));
        assert!(is_greeting_text("How are you?"));

        // Must not classify coding tasks as pure greetings
        assert!(!is_greeting_text("Hello, write me a web server in Rust"));
        assert!(!is_greeting_text("fn main() { println!(); }"));
        assert!(!is_greeting_text("Fix bug in code"));
    }

    #[test]
    fn test_lm_studio_apply_to_request_suppression() {
        // Binary on/off profile (e.g. Qwen, DeepSeek R1 GGUF on LM Studio)
        // Must NOT set reasoning_effort at all to avoid LM Studio warnings
        let prof_bin = ThinkingProfile {
            presets: vec!["off".to_string(), "on".to_string()],
            protocol: ThinkingProtocol::LmStudio,
            supported: true,
            default_preset: Some("on".to_string()),
        };
        let mut body = serde_json::json!({});
        prof_bin.apply_to_request(&mut body, "off");
        assert_eq!(body.get("reasoning_effort"), None);
        assert_eq!(body["reasoning"], "off");
        assert_eq!(body["enable_thinking"], false);
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);

        prof_bin.apply_to_request(&mut body, "on");
        assert_eq!(body.get("reasoning_effort"), None);
        assert_eq!(body["reasoning"], "on");
        assert_eq!(body["enable_thinking"], true);
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], true);

        // Tiered profile (LM Studio models reporting low/medium/high presets)
        let prof_tiered = ThinkingProfile {
            presets: vec!["off".to_string(), "low".to_string(), "medium".to_string(), "high".to_string()],
            protocol: ThinkingProtocol::LmStudio,
            supported: true,
            default_preset: Some("medium".to_string()),
        };
        let mut body_t = serde_json::json!({});
        prof_tiered.apply_to_request(&mut body_t, "off");
        assert_eq!(body_t["reasoning_effort"], "off");
        assert_eq!(body_t["reasoning"], "off");
        assert_eq!(body_t["enable_thinking"], false);

        prof_tiered.apply_to_request(&mut body_t, "medium");
        assert_eq!(body_t["reasoning_effort"], "medium");
        assert_eq!(body_t["reasoning"], "medium");
        assert_eq!(body_t["enable_thinking"], true);
    }

    #[test]
    fn test_resolve_dynamic_binary_and_tiered() {
        use crate::types::ChatMessage;

        // Binary profile (e.g. LM Studio deepseek-r1 / qwen)
        let binary_prof = ThinkingProfile {
            presets: vec!["off".to_string(), "on".to_string()],
            protocol: ThinkingProtocol::LmStudio,
            supported: true,
            default_preset: Some("on".to_string()),
        };

        let hello = vec![ChatMessage::user("Hello")];
        assert_eq!(binary_prof.resolve_dynamic(&hello), Some("off"));

        let code = vec![ChatMessage::user("Fix compilation error in main.rs")];
        assert_eq!(binary_prof.resolve_dynamic(&code), Some("on"));

        // Tiered profile (e.g. low/medium/high)
        let tiered_prof = ThinkingProfile {
            presets: vec!["low".to_string(), "medium".to_string(), "high".to_string()],
            protocol: ThinkingProtocol::ReasoningEffort,
            supported: true,
            default_preset: Some("medium".to_string()),
        };

        assert_eq!(tiered_prof.resolve_dynamic(&hello), Some("low"));
        assert_eq!(tiered_prof.resolve_dynamic(&code), Some("high"));
    }
}
