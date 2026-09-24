//! Reasoning presets differ per API and model (`[off, on]`, `[low, medium,
//! high]`, `[low, high, xhigh]`, token budgets). They are discovered from model
//! metadata or error responses, not hardcoded.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThinkingProtocol {
    /// `reasoning_effort: "<preset>"`
    ReasoningEffort,
    /// `reasoning: { effort: "<preset>" }`
    ReasoningObject,
    /// `thinking: { type: "enabled"|"disabled", budget_tokens: N }`
    ThinkingObject,
    /// `enable_thinking: bool`
    BooleanFlag,
    /// `reasoning: "<preset>"` and `enable_thinking: bool`
    LmStudio,
    /// The server said nothing about reasoning. Only "off" is sent.
    Unreported,
    /// Google's OpenAI-compatible endpoint: `extra_body.google.thinking_config`
    /// with a level (Gemini 3) or a token budget (2.5), and `include_thoughts`,
    /// without which the thinking is never shown. It cannot go with
    /// `reasoning_effort`, so that is not sent.
    Gemini,
}

/// Decided by the API that answered, not by the address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ServerKind {
    LmStudio,
    /// Its model list says `owned_by: llamacpp`.
    LlamaCpp,
    #[default]
    Other,
}

impl ServerKind {
    /// Keeps a prompt cache and reads the thinking switches in the chat template.
    pub fn runs_local_models(self) -> bool {
        matches!(self, ServerKind::LmStudio | ServerKind::LlamaCpp)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredModel {
    /// E.g. `gemma-4-e2b-it-qat@q4_k_xl`.
    pub id: String,
    pub display_name: Option<String>,
    pub is_loaded: bool,
    pub context_length: Option<usize>,
    /// Maximum the architecture supports.
    pub max_context_length: Option<usize>,
    pub thinking: ThinkingProfile,
    pub supports_tools: bool,
    pub supports_vision: bool,
}

impl DiscoveredModel {
    /// E.g. `131k ctx`.
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

    /// E.g. `128k ctx · tools · thinking: on [off,on]`.
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

        if self.thinking.is_unreported() {
            // "no reasoning" would claim what the server did not say.
        } else if self.thinking.supported {
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
            // Only when the server said it: silence is not a "no".
            parts.push("no reasoning".to_string());
        }

        parts.join(" · ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ServerDiscovery {
    pub base_url: String,
    pub models: Vec<DiscoveredModel>,
    pub active_model: Option<DiscoveredModel>,
    #[serde(default)]
    pub kind: ServerKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingProfile {
    pub presets: Vec<String>,
    pub protocol: ThinkingProtocol,
    pub supported: bool,
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

/// Ordered, so callers can compare levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TaskComplexity {
    Minimal,
    Low,
    Medium,
    /// Code, file edits, debugging.
    High,
}

impl TaskComplexity {
    /// Saturates rather than wrapping: a model that overthinks greetings cannot be
    /// pushed below "no thinking".
    pub fn shifted(self, steps: i8) -> Self {
        let ladder = [Self::Minimal, Self::Low, Self::Medium, Self::High];
        let at = ladder.iter().position(|c| *c == self).unwrap_or(0) as i32;
        let moved = (at + steps as i32).clamp(0, ladder.len() as i32 - 1) as usize;
        ladder[moved]
    }
}

impl ThinkingProfile {
    pub fn unsupported() -> Self {
        Self {
            presets: Vec::new(),
            protocol: ThinkingProtocol::ReasoningEffort,
            supported: false,
            default_preset: None,
        }
    }

    /// Unlike [`Self::unsupported`], this claims nothing: no presets are offered,
    /// but turning reasoning off is still honoured.
    pub fn unreported() -> Self {
        Self {
            presets: Vec::new(),
            protocol: ThinkingProtocol::Unreported,
            supported: false,
            default_preset: None,
        }
    }

    pub fn is_unreported(&self) -> bool {
        self.protocol == ThinkingProtocol::Unreported
    }

    pub fn min_effort(&self) -> Option<&str> {
        if !self.supported || self.presets.is_empty() {
            return None;
        }
        for disabled in &["off", "none", "disabled", "false", "0"] {
            if let Some(p) = self.presets.iter().find(|p| p.eq_ignore_ascii_case(disabled)) {
                return Some(p.as_str());
            }
        }
        // These protocols always accept "off", listed or not.
        if self.protocol == ThinkingProtocol::BooleanFlag || self.protocol == ThinkingProtocol::LmStudio {
            return Some("off");
        }
        for low in &["low", "minimal", "min", "fast"] {
            if let Some(p) = self.presets.iter().find(|p| p.eq_ignore_ascii_case(low)) {
                return Some(p.as_str());
            }
        }
        self.presets.first().map(String::as_str)
    }

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

    pub fn resolve_effort(&self, intent: crate::types::ThinkingEffort) -> Option<&str> {
        match intent {
            crate::types::ThinkingEffort::Auto => {
                // Auto without message context: the default preset, else the max.
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

    pub fn resolve_dynamic(&self, messages: &[crate::types::ChatMessage]) -> Option<&str> {
        self.resolve_dynamic_biased(messages, 0)
    }

    /// `bias` -1 thinks one preset less, +1 one more.
    pub fn resolve_dynamic_biased(
        &self,
        messages: &[crate::types::ChatMessage],
        bias: i8,
    ) -> Option<&str> {
        if !self.supported || self.presets.is_empty() {
            return None;
        }
        let complexity = Self::analyze_turn_complexity(messages).shifted(bias);
        self.resolve_for_complexity(complexity)
    }

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
                        // Binary on/off models keep "on": flipping enable_thinking evicts the KV
                        // prefix cache.
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

    pub fn apply_to_request(&self, body: &mut serde_json::Value, effort: &str) {
        let is_off = effort == "off" || effort == "disabled" || effort == "none" || effort == "false" || effort == "0";
        match self.protocol {
            ThinkingProtocol::Unreported => {}
            ThinkingProtocol::Gemini => {
                let model = body["model"].as_str().unwrap_or_default().to_string();
                body["extra_body"] = serde_json::json!({ "google": { "thinking_config": gemini_thinking_config(&model, effort) } });
            }
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

/// The level is decided by the user's own message and held for every step of
/// the task: switching presets between steps costs the backend its prefix
/// cache. Only a failed tool result raises it.
///
/// Signals are structural, not keyword lists, so Russian prompts are scored
/// the same as English ones; length is counted in characters, not bytes.
pub fn analyze_turn_complexity(messages: &[crate::types::ChatMessage]) -> TaskComplexity {
    use crate::types::Role;

    let Some(last) = messages.last() else {
        return TaskComplexity::Medium;
    };

    // A failed step is worth thinking about, whatever the task looked like.
    let last_failed = last.role == Role::Tool && Self::looks_like_failure(&last.content);

    // The task's own message, not tool output and not a "yes" given mid-task.
    let mut base = messages
        .iter()
        .rev()
        .filter(|m| m.role == Role::User)
        .find(|m| !Self::is_bare_acknowledgement(Self::user_text(&m.content)))
        .map(|m| Self::user_message_complexity(&m.content))
        .unwrap_or(TaskComplexity::Minimal);

    // "do it" approves what was just proposed, and the proposal is the task.
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

fn raise(level: TaskComplexity) -> TaskComplexity {
    match level {
        TaskComplexity::Minimal => TaskComplexity::Low,
        TaskComplexity::Low => TaskComplexity::Medium,
        TaskComplexity::Medium | TaskComplexity::High => TaskComplexity::High,
    }
}

/// The markers are program output, so this works in any language.
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
        || (lower.contains("exit code:") && !lower.contains("exit code: 0"))
}

/// The user overriding the guess, in Russian or English. Wins outright.
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

/// Without the memory block injected ahead of the message.
fn user_text(raw: &str) -> &str {
    match raw.rsplit_once("\n\n---\n\n") {
        Some((_memory, user_part)) => user_part.trim(),
        None => raw.trim(),
    }
}

/// "yes", "1", "ok", "go on" in any language: an answer, not a task.
fn is_bare_acknowledgement(prompt: &str) -> bool {
    prompt.chars().count() <= 12
        && prompt.split_whitespace().count() <= 2
        && !prompt.contains('?')
}

fn user_message_complexity(raw: &str) -> TaskComplexity {
    let prompt = Self::user_text(raw);
    let lower = prompt.to_lowercase();

    if let Some(explicit) = Self::explicit_override(&lower) {
        return explicit;
    }
    if crate::thinking::is_greeting_text(prompt) {
        return TaskComplexity::Minimal;
    }

    // Characters, not bytes: Cyrillic is two bytes per letter.
    let chars = prompt.chars().count();

    if Self::is_bare_acknowledgement(prompt) {
        return TaskComplexity::Minimal;
    }

    let mut score = 0i32;

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

    if prompt.contains("```") {
        score += 2;
    }
    let code_marks = ["::", "->", "=>", "fn ", "def ", "class ", "impl ", "struct ", "()", "{}", "<T>"];
    if code_marks.iter().filter(|m| prompt.contains(**m)).count() >= 2 {
        score += 2;
    }

    let has_path = prompt.split_whitespace().any(|w| {
        let w = w.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '/' && c != '_');
        (w.contains('/') && !w.contains("://")) || w.rsplit_once('.').is_some_and(|(stem, ext)| {
            !stem.is_empty() && (2..=4).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphabetic())
        })
    });
    if has_path {
        score += 1;
    }

    const TROUBLE: &[&str] = &[
        "error", "panic", "crash", "fails", "failing", "broken", "bug", "regress", "why does",
        "ошибк", "падает", "ломает", "не работает", "баг", "почему",
    ];
    if TROUBLE.iter().any(|k| lower.contains(k)) {
        score += 2;
    }

    // The one lexical signal kept: a request to produce or change something.
    // Short and bilingual on purpose; the old forty-word English list fired on
    // "run" and "check" in nearly every sentence.
    const MAKE: &[&str] = &[
        "write ", "create ", "implement ", "add ", "fix ", "refactor", "rename", "delete ",
        "remove ", "update ", "migrate", "port ", "optimis", "optimiz",
        "напиш", "создай", "добавь", "исправь", "почини", "переимен", "удали", "обнови",
        "рефактор", "реализуй", "перепиш",
    ];
    if MAKE.iter().any(|k| lower.contains(k)) {
        score += 1;
    }

    // Same verb and length, very different scope.
    const WHOLE: &[&str] = &[
        "whole project", "entire project", "whole codebase", "entire codebase", "all files",
        "everywhere", "across the codebase", "the whole thing", "from scratch", "rewrite everything",
        "весь проект", "всего проекта", "всему проекту", "полный", "полностью", "везде",
        "во всех файлах", "с нуля", "всё приложение", "все файлы",
    ];
    if WHOLE.iter().any(|k| lower.contains(k)) {
        score += 2;
    }

    if chars > 240 {
        score += 2;
    } else if chars > 80 {
        score += 1;
    }

    if prompt.contains('?') {
        score += 1;
    }

    match score {
        // Nothing suggests work: short is small talk, long is still a question.
        0 if chars <= 40 => TaskComplexity::Minimal,
        0 => TaskComplexity::Low,
        1..=2 => TaskComplexity::Medium,
        _ => TaskComplexity::High,
    }
}


    /// From a `/v1/models` or `/v1/models/{model}` response.
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

    /// Learns supported presets from an API error.
    pub fn parse_api_error(error_body: &str) -> Option<Self> {
        let err = error_body.to_lowercase();
        // Only an error about thinking controls can teach presets; any other 400
        // ("invalid messages[3].content") must not be mined for bracketed lists.
        if !["reasoning", "thinking", "effort", "supported settings"].iter().any(|k| err.contains(k)) {
            return None;
        }

        if err.contains("unrecognized request argument: reasoning_effort")
            || err.contains("unknown parameter: reasoning_effort")
            || (err.contains("extra inputs are not permitted") && err.contains("reasoning"))
            || err.contains("unsupported parameter: reasoning")
        {
            return Some(Self::unsupported());
        }

        // `['low', 'medium', 'high']` or `[off, on]`: a flat list of plain words.
        // A FastAPI error also has `"loc":["body","reasoning_effort"]`, which
        // names the field, not its values.
        let bracketed = err.match_indices('[').find_map(|(start, _)| {
            let inside = &err[start + 1..start + 1 + err[start + 1..].find(']')?];
            let before = err[..start].trim_end().trim_end_matches(':').trim_end().trim_end_matches('"');
            if inside.contains('[') || before.ends_with("loc") {
                return None;
            }
            let words: Vec<String> =
                inside.split(',').map(|s| s.trim().trim_matches('\'').trim_matches('"').trim().to_string()).collect();
            let plain = |s: &String| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            words.iter().all(plain).then_some(words)
        });
        if let Some(presets) = bracketed {
            let protocol = if err.contains("supported settings") || (presets.contains(&"off".to_string()) && presets.contains(&"on".to_string())) {
                ThinkingProtocol::LmStudio
            } else if err.contains("reasoning_effort") {
                ThinkingProtocol::ReasoningEffort
            } else if err.contains("reasoning") {
                ThinkingProtocol::ReasoningObject
            } else {
                ThinkingProtocol::ReasoningEffort
            };
            return Some(Self { presets, protocol, supported: true, default_preset: None });
        }

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
        if words.len() == 1 {
            return true;
        }
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

/// Embedding models cannot chat, so they are never offered.
fn is_embedding(m: &serde_json::Value) -> bool {
    m.get("type")
        .and_then(|v| v.as_str())
        .is_some_and(|t| t.to_ascii_lowercase().starts_with("embedding"))
}

/// LM Studio's `/api/v1/models` has richer capabilities but can omit models
/// (the loaded one, on a real server); `/api/v0/models` lists every model with
/// its load state. What the first listing says is kept.
pub fn merge_server_models(mut primary: Vec<DiscoveredModel>, other: Vec<DiscoveredModel>) -> Vec<DiscoveredModel> {
    for m in other {
        match primary.iter_mut().find(|p| p.id == m.id) {
            Some(p) => {
                if m.is_loaded && !p.is_loaded {
                    p.is_loaded = true;
                    p.context_length = m.context_length.or(p.context_length);
                }
                p.supports_vision |= m.supports_vision;
                if p.thinking.is_unreported() && !m.thinking.is_unreported() {
                    p.thinking = m.thinking.clone();
                }
                p.max_context_length = p.max_context_length.or(m.max_context_length);
            }
            None => primary.push(m),
        }
    }
    primary
}

/// LM Studio `/api/v1/models`, `/api/v0/models`, or OpenAI `/v1/models`.
/// What Gemini takes for a preset. 2.5 counts a budget of tokens (0 is off,
/// -1 lets the model decide); 3 and later take the level by name.
pub fn gemini_thinking_config(model: &str, effort: &str) -> serde_json::Value {
    let effort = effort.to_ascii_lowercase();
    if model.contains("gemini-2.5") {
        let budget: i64 = match effort.as_str() {
            "none" | "off" | "disabled" | "0" => 0,
            "minimal" | "low" => 1024,
            "medium" => 8192,
            "high" | "max" | "xhigh" => 24576,
            _ => -1,
        };
        return serde_json::json!({ "thinking_budget": budget, "include_thoughts": budget != 0 });
    }
    match effort.as_str() {
        "minimal" | "low" | "medium" | "high" => serde_json::json!({ "thinking_level": effort, "include_thoughts": true }),
        _ => serde_json::json!({ "include_thoughts": true }),
    }
}

/// The levels each Gemini family takes, from Google's compatibility notes:
/// only 2.5 Flash and Flash-Lite can turn thinking off, and 3.x Pro has no
/// `minimal`.
pub fn gemini_profile(model: &str, thinks: bool) -> ThinkingProfile {
    if !thinks {
        return ThinkingProfile::unsupported();
    }
    let presets: &[&str] = if model.contains("gemini-2.5") {
        if model.contains("pro") { &["low", "medium", "high"] } else { &["none", "low", "medium", "high"] }
    } else if model.contains("pro") {
        &["low", "medium", "high"]
    } else {
        &["minimal", "low", "medium", "high"]
    };
    ThinkingProfile {
        presets: presets.iter().map(|p| p.to_string()).collect(),
        protocol: ThinkingProtocol::Gemini,
        supported: true,
        default_preset: None,
    }
}

/// Gemini's own model list (`/v1beta/models`): the compatible one has no
/// context sizes and says nothing of thinking. Chat models only.
pub fn parse_gemini_models(data: &serde_json::Value) -> Vec<DiscoveredModel> {
    let Some(models) = data.get("models").and_then(|m| m.as_array()) else { return Vec::new() };
    models
        .iter()
        .filter(|m| {
            m.get("supportedGenerationMethods")
                .and_then(|g| g.as_array())
                .is_some_and(|g| g.iter().any(|x| x == "generateContent"))
        })
        .filter_map(|m| {
            let id = m.get("name")?.as_str()?.to_string();
            let thinks = m.get("thinking").and_then(|t| t.as_bool()).unwrap_or(false);
            let context = m.get("inputTokenLimit").and_then(|v| v.as_u64()).map(|n| n as usize);
            Some(DiscoveredModel {
                display_name: m.get("displayName").and_then(|v| v.as_str()).map(str::to_string),
                is_loaded: false,
                context_length: context,
                max_context_length: context,
                thinking: gemini_profile(&id, thinks),
                supports_tools: true,
                supports_vision: true,
                id,
            })
        })
        .collect()
}

pub fn parse_server_models(data: &serde_json::Value) -> Vec<DiscoveredModel> {
    let mut discovered = Vec::new();

    // Gemini's own list: `{"models": [{"name": "models/…", "inputTokenLimit": …}]}`.
    let is_gemini = data
        .get("models")
        .and_then(|m| m.as_array())
        .is_some_and(|m| m.iter().any(|x| x.get("inputTokenLimit").is_some() && x.get("name").is_some()));
    if is_gemini {
        return parse_gemini_models(data);
    }

    // LM Studio v1: `{"models": [...]}`
    if let Some(models) = data.get("models").and_then(|m| m.as_array()) {
        for m in models {
            let id = m.get("key")
                .or_else(|| m.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            if id.is_empty() || is_embedding(m) {
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
            let mut supports_vision = m
                .get("type")
                .and_then(|v| v.as_str())
                .map(|t| t.eq_ignore_ascii_case("vlm"))
                .unwrap_or(false);
            // v1 names the reasoning settings of every model that has them, so a model
            // described without any cannot reason. An undescribed model is unknown.
            let mut thinking = if m.get("capabilities").is_some() {
                ThinkingProfile::unsupported()
            } else {
                ThinkingProfile::unreported()
            };

            if let Some(caps) = m.get("capabilities") {
                supports_tools = caps.get("trained_for_tool_use").and_then(|v| v.as_bool()).unwrap_or(false);
                supports_vision |= caps.get("vision").and_then(|v| v.as_bool()).unwrap_or(false);

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

    // LM Studio v0 or OpenAI: `{"data": [...]}` or a bare array.
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
        if id.is_empty() || is_embedding(m) {
            continue;
        }

        let is_loaded = m.get("state").and_then(|v| v.as_str()).map(|s| s == "loaded").unwrap_or(false);
        // llama-server: `meta` has both the running and the trained context.
        let meta = m.get("meta");
        let context_length = m.get("loaded_context_length")
            .or_else(|| m.get("context_length"))
            .or_else(|| meta.and_then(|x| x.get("n_ctx")))
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);
        let max_context_length = m.get("max_context_length")
            .or_else(|| meta.and_then(|x| x.get("n_ctx_train")))
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);

        let mut supports_tools = false;
        // LM Studio marks vision by type "vlm", not always in capabilities.
        let mut supports_vision = m
            .get("type")
            .and_then(|v| v.as_str())
            .map(|t| t.eq_ignore_ascii_case("vlm"))
            .unwrap_or(false);
        // v0 and plain OpenAI listings say nothing about reasoning unless a
        // capability object lists effort levels.
        let mut thinking = ThinkingProfile::unreported();

        if let Some(caps) = m.get("capabilities") {
            if let Some(arr) = caps.as_array() {
                supports_tools = arr.iter().any(|v| v.as_str() == Some("tool_use"));
                supports_vision |= arr.iter().any(|v| matches!(v.as_str(), Some("vision") | Some("image_input")));
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

        discovered.push(DiscoveredModel {
            id: id.to_string(),
            display_name: None,
            is_loaded,
            context_length,
            max_context_length,
            thinking,
            supports_tools,
            supports_vision,
        });
    }

    discovered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_models_bring_their_context_and_thinking_levels() {
        let native = serde_json::json!({ "models": [
            { "name": "models/gemini-2.5-flash", "inputTokenLimit": 1048576, "thinking": true,
              "supportedGenerationMethods": ["generateContent", "countTokens"] },
            { "name": "models/gemini-2.5-pro", "inputTokenLimit": 1048576, "thinking": true,
              "supportedGenerationMethods": ["generateContent"] },
            { "name": "models/gemini-3-flash", "inputTokenLimit": 1048576, "thinking": true,
              "supportedGenerationMethods": ["generateContent"] },
            { "name": "models/text-embedding-004", "inputTokenLimit": 2048,
              "supportedGenerationMethods": ["embedContent"] }
        ] });
        let models = parse_server_models(&native);
        assert_eq!(models.len(), 3, "embedding models are not chat models");
        assert_eq!(models[0].context_length, Some(1048576));
        assert_eq!(models[0].thinking.presets, vec!["none", "low", "medium", "high"]);
        assert_eq!(models[1].thinking.presets, vec!["low", "medium", "high"], "2.5 Pro cannot turn thinking off");
        assert_eq!(models[2].thinking.presets, vec!["minimal", "low", "medium", "high"]);

        assert_eq!(gemini_thinking_config("models/gemini-2.5-flash", "none"), serde_json::json!({ "thinking_budget": 0, "include_thoughts": false }));
        assert_eq!(gemini_thinking_config("models/gemini-2.5-pro", "high")["thinking_budget"], 24576);
        assert_eq!(gemini_thinking_config("models/gemini-3-flash", "low"), serde_json::json!({ "thinking_level": "low", "include_thoughts": true }));
        let mut body = serde_json::json!({ "model": "models/gemini-3-flash" });
        models[2].thinking.apply_to_request(&mut body, "high");
        assert_eq!(body["extra_body"]["google"]["thinking_config"]["thinking_level"], "high");
        assert!(body.get("reasoning_effort").is_none(), "it cannot go together with thinking_config");
    }

    #[test]
    fn a_fastapi_field_location_is_not_taken_for_the_allowed_values() {
        let err = r#"{"detail":[{"type":"literal_error","loc":["body","reasoning_effort"],"msg":"Input should be 'low', 'medium' or 'high'"}]}"#;
        let learned = ThinkingProfile::parse_api_error(err);
        assert!(learned.as_ref().is_none_or(|p| !p.presets.contains(&"reasoning_effort".to_string())), "{learned:?}");
        let listed = ThinkingProfile::parse_api_error("reasoning_effort must be one of ['low', 'medium', 'high']").unwrap();
        assert_eq!(listed.presets, vec!["low", "medium", "high"]);
    }

    #[test]
    fn a_model_is_not_given_reasoning_settings_because_of_its_name() {
        // A model name alone ("qwen", "think") is not evidence of reasoning support.
        let v0 = serde_json::json!({"data":[{"id":"qwen3.6-35b-a3b-mtp","type":"vlm","arch":"qwen35moe","state":"loaded","capabilities":["tool_use"]}]});
        let m = &parse_server_models(&v0)[0];
        assert!(!m.thinking.supported);
        assert!(m.thinking.is_unreported(), "the server said nothing, so nothing is known");

        let named_to_tempt = serde_json::json!({"data":[{"id":"deepseek-r1-thinking-reasoner-gemma-4"}]});
        assert!(parse_server_models(&named_to_tempt)[0].thinking.is_unreported());
    }

    #[test]
    fn a_model_lm_studio_describes_without_reasoning_cannot_reason() {
        let v1 = serde_json::json!({"models":[{"key":"qwen2.5-coder-7b","type":"llm","capabilities":{"vision":false,"trained_for_tool_use":true}}]});
        let m = &parse_server_models(&v1)[0];
        assert!(!m.thinking.supported);
        assert!(!m.thinking.is_unreported(), "v1 lists reasoning for every model that has it");
    }

    #[test]
    fn what_one_listing_said_about_reasoning_fills_a_listing_that_said_nothing() {
        let silent = parse_server_models(&serde_json::json!({"data":[{"id":"m","state":"loaded"}]}));
        let told = parse_server_models(&serde_json::json!({"models":[{"key":"m","capabilities":{"reasoning":{"allowed_options":["off","on"],"default":"on"}}}]}));
        let merged = merge_server_models(silent, told);
        assert!(merged[0].thinking.supported);
        assert_eq!(merged[0].thinking.presets, vec!["off", "on"]);
        assert!(merged[0].is_loaded, "what the first listing knew is kept");
    }

    #[test]
    fn llama_server_reports_the_context_it_runs_with_and_the_one_it_was_trained_for() {
        // Real llama-server b10630 listing, trimmed: both "models" and "data".
        let listing = serde_json::json!({
            "models": [{"name": "/models/gemma.gguf", "model": "/models/gemma.gguf", "capabilities": ["completion"]}],
            "object": "list",
            "data": [{"id": "/models/gemma.gguf", "object": "model", "owned_by": "llamacpp",
                      "meta": {"n_ctx": 2048, "n_ctx_train": 131072}}]
        });
        let models = parse_server_models(&listing);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "/models/gemma.gguf");
        assert_eq!(models[0].context_length, Some(2048));
        assert_eq!(models[0].max_context_length, Some(131072));
    }

    #[test]
    fn a_vision_model_is_recognised_however_the_server_says_so() {
        let by_type = serde_json::json!({"data":[{"id":"qwen3.6-35b","type":"vlm","capabilities":["tool_use"]}]});
        assert!(parse_server_models(&by_type)[0].supports_vision);

        let by_capability = serde_json::json!({"data":[{"id":"m","capabilities":["tool_use","vision"]}]});
        assert!(parse_server_models(&by_capability)[0].supports_vision);

        let text_only = serde_json::json!({"data":[{"id":"m","type":"llm","capabilities":["tool_use"]}]});
        assert!(!parse_server_models(&text_only)[0].supports_vision);
    }

    #[test]
    fn a_learned_correction_moves_one_preset_and_stops_at_the_ends() {
        use TaskComplexity::*;
        assert_eq!(Medium.shifted(-1), Low);
        assert_eq!(Medium.shifted(1), High);
        assert_eq!(Minimal.shifted(-1), Minimal, "there is nothing below not thinking");
        assert_eq!(High.shifted(1), High);
        assert_eq!(Medium.shifted(0), Medium);
    }

    #[test]
    fn the_correction_changes_the_preset_auto_picks() {
        let profile = ThinkingProfile {
            presets: vec!["off".into(), "low".into(), "medium".into(), "high".into()],
            protocol: ThinkingProtocol::ReasoningEffort,
            supported: true,
            default_preset: Some("medium".into()),
        };
        let ask = [crate::types::ChatMessage::user(
            "Refactor the whole project: split the loop into steps, then fix the failing tests in crates/core/src/loop_.rs",
        )];
        let neutral = profile.resolve_dynamic_biased(&ask, 0).map(str::to_string);
        let turned_down = profile.resolve_dynamic_biased(&ask, -1).map(str::to_string);
        assert_eq!(neutral.as_deref(), Some("high"), "a big task asks for the most thinking");
        assert_eq!(turned_down.as_deref(), Some("medium"), "the learned step is actually applied");
        assert_eq!(profile.resolve_dynamic(&ask).map(str::to_string), neutral, "no bias, no change");
    }

    #[test]
    fn test_min_and_max_effort_selection() {
        let p_binary = ThinkingProfile {
            presets: vec!["off".into(), "on".into()],
            protocol: ThinkingProtocol::BooleanFlag,
            supported: true,
            default_preset: Some("on".into()),
        };
        assert_eq!(p_binary.min_effort(), Some("off"));
        assert_eq!(p_binary.max_effort(), Some("on"));
        assert_eq!(p_binary.resolve_effort(crate::types::ThinkingEffort::Default), Some("on"));

        let p_four = ThinkingProfile {
            presets: vec!["off".into(), "low".into(), "medium".into(), "high".into()],
            protocol: ThinkingProtocol::ReasoningEffort,
            supported: true,
            default_preset: None,
        };
        assert_eq!(p_four.min_effort(), Some("off"));
        assert_eq!(p_four.max_effort(), Some("high"));

        let p_classic = ThinkingProfile {
            presets: vec!["low".into(), "medium".into(), "high".into()],
            protocol: ThinkingProtocol::ReasoningEffort,
            supported: true,
            default_preset: None,
        };
        assert_eq!(p_classic.min_effort(), Some("low"));
        assert_eq!(p_classic.max_effort(), Some("high"));

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
        let err1 = "Invalid reasoning_effort 'off'. Supported values are: ['low', 'medium', 'high']";
        let p1 = ThinkingProfile::parse_api_error(err1).expect("parsed p1");
        assert_eq!(p1.presets, vec!["low", "medium", "high"]);
        assert_eq!(p1.min_effort(), Some("low"));

        let err2 = "reasoning_effort must be one of: ['low', 'high', 'xhigh']";
        let p2 = ThinkingProfile::parse_api_error(err2).expect("parsed p2");
        assert_eq!(p2.presets, vec!["low", "high", "xhigh"]);
        assert_eq!(p2.max_effort(), Some("xhigh"));

        let err3 = "Invalid value for reasoning. Expected one of: [off, on]";
        let p3 = ThinkingProfile::parse_api_error(err3).expect("parsed p3");
        assert_eq!(p3.presets, vec!["off", "on"]);
        assert_eq!(p3.min_effort(), Some("off"));

        let err4 = "Unrecognized request argument: reasoning_effort";
        let p4 = ThinkingProfile::parse_api_error(err4).expect("parsed p4");
        assert!(!p4.supported);
        assert_eq!(p4.min_effort(), None);

        let err5 = "Reasoning setting 'high' is not supported by model. Supported settings: 'on', 'off'. Falling back to reasoning setting 'on'.";
        let p5 = ThinkingProfile::parse_api_error(err5).expect("parsed p5");
        assert_eq!(p5.presets, vec!["on", "off"]);
        assert_eq!(p5.protocol, ThinkingProtocol::LmStudio);
    }

    #[test]
    fn a_model_the_v1_listing_leaves_out_is_taken_from_v0() {
        // Real LM Studio shapes: v1 without the loaded qwen, v0 with it.
        let v1 = serde_json::json!({"models": [
            {"type": "llm", "key": "gemma-4-e2b-it-qat@q4_k_xl", "loaded_instances": [], "max_context_length": 131072,
             "capabilities": {"vision": false, "trained_for_tool_use": true,
                              "reasoning": {"allowed_options": ["off", "on"], "default": "on"}}},
            {"type": "embedding", "key": "text-embedding-nomic-embed-text-v1.5", "loaded_instances": [], "max_context_length": 2048}
        ]});
        let v0 = serde_json::json!({"data": [
            {"id": "qwen3.6-35b-a3b-mtp", "type": "vlm", "state": "loaded", "max_context_length": 262144},
            {"id": "gemma-4-e2b-it-qat@q4_k_xl", "type": "llm", "state": "not-loaded", "max_context_length": 131072},
            {"id": "text-embedding-nomic-embed-text-v1.5", "type": "embeddings", "state": "not-loaded", "max_context_length": 2048}
        ]});
        let merged = merge_server_models(parse_server_models(&v1), parse_server_models(&v0));
        let ids: Vec<&str> = merged.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["gemma-4-e2b-it-qat@q4_k_xl", "qwen3.6-35b-a3b-mtp"]);
        assert!(merged[1].is_loaded, "the loaded model is found and known to be loaded");
        assert!(merged[1].supports_vision);
        assert!(merged[0].supports_tools && merged[0].thinking.supported, "what v1 knew about a model it listed is kept");
    }

    #[test]
    fn a_model_loaded_according_to_v0_is_loaded_even_if_v1_listed_it_idle() {
        let v1 = serde_json::json!({"models": [{"type": "llm", "key": "m", "loaded_instances": [], "max_context_length": 131072}]});
        let v0 = serde_json::json!({"data": [{"id": "m", "type": "llm", "state": "loaded", "loaded_context_length": 32768}]});
        let merged = merge_server_models(parse_server_models(&v1), parse_server_models(&v0));
        assert_eq!(merged.len(), 1);
        assert!(merged[0].is_loaded);
        assert_eq!(merged[0].context_length, Some(32768));
    }

    #[test]
    fn an_embedding_model_is_never_offered_as_a_chat_model() {
        let v1 = serde_json::json!({"models": [
            {"type": "llm", "key": "chat", "loaded_instances": []},
            {"type": "embedding", "key": "text-embedding-nomic-embed-text-v1.5", "loaded_instances": []}
        ]});
        let v0 = serde_json::json!({"data": [
            {"id": "chat", "type": "llm", "state": "not-loaded"},
            {"id": "text-embedding-nomic-embed-text-v1.5", "type": "embeddings", "state": "not-loaded"}
        ]});
        for listing in [v1, v0] {
            let ids: Vec<String> = parse_server_models(&listing).into_iter().map(|m| m.id).collect();
            assert_eq!(ids, vec!["chat".to_string()], "{listing}");
        }
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

        let prof = ThinkingProfile::parse_model_metadata(&json, "gemma-4").expect("found profile");
        assert_eq!(prof.presets, vec!["off", "on"]);
    }

    #[test]
    fn greetings_and_acknowledgements_need_no_reasoning() {
        use crate::types::ChatMessage;
        for text in ["Hello!", "thanks, ok", "What's up bro!", "Good morning", "ок", "да"] {
            assert_eq!(
                ThinkingProfile::analyze_turn_complexity(&[ChatMessage::user(text)]),
                TaskComplexity::Minimal,
                "{text}"
            );
        }
        assert_eq!(
            ThinkingProfile::analyze_turn_complexity(&[ChatMessage::user(
                "# Memory (automatically loaded)\n\n---\n\nHello"
            )]),
            TaskComplexity::Minimal
        );
    }

    #[test]
    fn a_russian_prompt_is_judged_like_the_same_prompt_in_english() {
        use crate::types::ChatMessage;
        // 57 characters, 94 bytes: counted as "long" when length was in bytes.
        let ru = ThinkingProfile::analyze_turn_complexity(&[ChatMessage::user(
            "Прочитай src/parser.rs и коротко опиши что делает функция",
        )]);
        let en = ThinkingProfile::analyze_turn_complexity(&[ChatMessage::user(
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
            let level = ThinkingProfile::analyze_turn_complexity(&[ChatMessage::user(text)]);
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
                ThinkingProfile::analyze_turn_complexity(&[ChatMessage::user(text)]),
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
                ThinkingProfile::analyze_turn_complexity(&[ChatMessage::user(brief)]),
                TaskComplexity::Minimal,
                "{brief}"
            );
        }
        assert_eq!(
            ThinkingProfile::analyze_turn_complexity(&[ChatMessage::user("Подробно разбери архитектуру цикла")]),
            TaskComplexity::High
        );
    }

    #[test]
    fn agreeing_to_a_big_job_inherits_the_job() {
        use crate::types::ChatMessage;

        let stated = vec![
            ChatMessage::user("Делаем полный рефактор проекта"),
            ChatMessage::assistant("Хорошо, начну с разбора зависимостей. Приступать?"),
            ChatMessage::user("делай"),
        ];
        assert_eq!(ThinkingProfile::analyze_turn_complexity(&stated), TaskComplexity::High);

        let proposed = vec![
            ChatMessage::user("Что тут можно улучшить?"),
            ChatMessage::assistant(
                "Предлагаю полный рефактор проекта: вынести цикл в отдельный модуль, \
                 переписать разбор аргументов и прогнать тесты. Делаем?",
            ),
            ChatMessage::user("да"),
        ];
        assert_eq!(ThinkingProfile::analyze_turn_complexity(&proposed), TaskComplexity::High);
    }

    #[test]
    fn scope_counts_even_when_the_sentence_is_short() {
        use crate::types::ChatMessage;
        let one_file = ThinkingProfile::analyze_turn_complexity(&[ChatMessage::user("Refactor the parser")]);
        let whole = ThinkingProfile::analyze_turn_complexity(&[ChatMessage::user("Refactor the whole project")]);
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
            images: Vec::new(),
        };
        let task = ChatMessage::user("Read src/parser.rs and describe it");
        let base = ThinkingProfile::analyze_turn_complexity(std::slice::from_ref(&task));

        // Switching presets between steps costs the prefix cache.
        assert_eq!(
            ThinkingProfile::analyze_turn_complexity(&[task.clone(), tool("exit code: 0\noutput:\ndone")]),
            base
        );

        for failure in [
            "error: no such file: src/confg.rs",
            "exit code: 101\noutput:\ntest failures",
            "panicked at src/main.rs:42",
        ] {
            assert!(
                ThinkingProfile::analyze_turn_complexity(&[task.clone(), tool(failure)]) > base,
                "{failure}"
            );
        }
    }

    #[test]
    fn the_level_holds_across_the_steps_of_one_task() {
        use crate::types::{ChatMessage, Role};
        let history = vec![
            ChatMessage::user("Fix the compilation error in src/main.rs, then run the tests"),
            ChatMessage::assistant("Found a type error. Fix it now?"),
            ChatMessage::user("Yes"),
        ];
        assert_eq!(ThinkingProfile::analyze_turn_complexity(&history), TaskComplexity::High);

        let mut deep = history.clone();
        deep.push(ChatMessage {
            role: Role::Tool,
            content: "exit code: 0".into(),
            reasoning: None,
            tool_call_id: Some("2".into()),
            tool_calls: vec![],
            images: Vec::new(),
        });
        assert_eq!(
            ThinkingProfile::analyze_turn_complexity(&deep),
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

        assert!(!is_greeting_text("Hello, write me a web server in Rust"));
        assert!(!is_greeting_text("fn main() { println!(); }"));
        assert!(!is_greeting_text("Fix bug in code"));
    }

    #[test]
    fn test_lm_studio_apply_to_request_suppression() {
        // Must not set reasoning_effort: LM Studio warns about it.
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
