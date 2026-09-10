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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
                    body["chat_template_kwargs"] = serde_json::json!({ "enable_thinking": false });
                    body["chat_template_config"] = serde_json::json!({ "enable_thinking": false });
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

/// Analyze the turn messages to classify task complexity for dynamic reasoning.
pub fn analyze_turn_complexity(messages: &[crate::types::ChatMessage]) -> TaskComplexity {
    let last_msg = match messages.last() {
        Some(m) => m,
        None => return TaskComplexity::Medium,
    };

    match last_msg.role {
        crate::types::Role::Tool => {
            let content = last_msg.content.trim();
            // Checking if tool output indicates an error or failure
            let is_err = content.starts_with("Error:")
                || content.starts_with("error:")
                || content.contains("Traceback (most recent call last)")
                || content.contains("panicked at")
                || content.contains("BUILD FAILED")
                || content.contains("FAILED");
            if is_err {
                return TaskComplexity::High;
            }
            // Code or substantial file inspection requires reasoning
            if content.len() > 350
                || content.contains("fn ")
                || content.contains("struct ")
                || content.contains("impl ")
                || content.contains("def ")
                || content.contains("class ")
            {
                return TaskComplexity::High;
            }
            // Short status or acknowledgement from tool execution (e.g. "Applied edits to...")
            if content.len() <= 120 {
                return TaskComplexity::Low;
            }
            TaskComplexity::Medium
        }
        crate::types::Role::User => {
            let raw_content = &last_msg.content;
            // Strip injected memory block preamble if present
            let prompt = if let Some((_mem, user_part)) = raw_content.rsplit_once("\n\n---\n\n") {
                user_part.trim()
            } else {
                raw_content.trim()
            };

            let p_lower = prompt.to_lowercase();
            let clean_lower = prompt
                .trim_matches(|c: char| !c.is_alphanumeric() && !c.is_whitespace())
                .to_lowercase();

            // 1. Direct user brevity overrides: explicitly instructed not to think or answer in one line
            let is_explicit_brevity = p_lower.contains("без мыслей")
                || p_lower.contains("без рассуждений")
                || p_lower.contains("не думай")
                || p_lower.contains("ответь коротко")
                || p_lower.contains("ответь одной строкой")
                || p_lower.contains("без лишних слов")
                || p_lower.contains("no thinking")
                || p_lower.contains("don't think")
                || p_lower.contains("no reasoning")
                || p_lower.contains("concise")
                || p_lower.contains("one line")
                || p_lower.contains("briefly");
            if is_explicit_brevity {
                return TaskComplexity::Minimal;
            }

            // 2. Direct user deep reasoning overrides: explicitly instructed to reason or think deeply
            let is_explicit_deep = p_lower.contains("подумай")
                || p_lower.contains("поразмышляй")
                || p_lower.contains("проанализируй подробно")
                || p_lower.contains("рассуждай пошагово")
                || p_lower.contains("think step by step")
                || p_lower.contains("deep reasoning")
                || p_lower.contains("think deeply")
                || p_lower.contains("reason carefully")
                || p_lower.contains("full analysis");
            if is_explicit_deep {
                return TaskComplexity::High;
            }

            // 3. Pure greetings and casual pleasantries (never task execution)
            let is_greeting = is_greeting_text(prompt);

            // 4. Check follow-up confirmation or response to an ongoing task / question
            let is_confirmation_or_choice = matches!(
                clean_lower.as_str(),
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"
                | "да" | "нет" | "ок" | "окей" | "хорошо" | "делай" | "продолжай" | "погнали" | "давай" | "согласен" | "ладно" | "плюс" | "го"
                | "yes" | "no" | "y" | "n" | "ok" | "okay" | "sure" | "proceed" | "continue" | "go" | "do it" | "agree"
            );

            // If there is prior conversation history and user is answering a question or continuing a task
            if messages.len() > 1 && !is_greeting && (is_confirmation_or_choice || clean_lower.len() <= 40) {
                let prev_msgs = &messages[..messages.len() - 1];
                let has_active_task = prev_msgs.iter().rev().take(4).any(|m| {
                    m.role == crate::types::Role::Tool
                        || !m.tool_calls.is_empty()
                        || m.content.contains('?')
                        || m.content.contains("```")
                        || m.content.contains("error")
                        || m.content.contains("ошибк")
                        || m.content.contains("test")
                        || m.content.contains("тест")
                        || m.content.contains("fn ")
                        || m.content.contains("struct ")
                        || m.content.contains("diff")
                });

                if has_active_task {
                    // Continuing an active task -> preserve High complexity to keep thinking on and prevent KV cache eviction
                    return TaskComplexity::High;
                }
            }

            // 5. Technical task keywords & actions
            let has_tech_keywords = p_lower.contains("напиши")
                || p_lower.contains("создай")
                || p_lower.contains("исправь")
                || p_lower.contains("почини")
                || p_lower.contains("отрефактори")
                || p_lower.contains("рефактор")
                || p_lower.contains("перепиши")
                || p_lower.contains("добавь")
                || p_lower.contains("удали")
                || p_lower.contains("запусти")
                || p_lower.contains("проверь")
                || p_lower.contains("сборк")
                || p_lower.contains("ошибк")
                || p_lower.contains("паник")
                || p_lower.contains("баг")
                || p_lower.contains("тест")
                || p_lower.contains("архитектур")
                || p_lower.contains("проект")
                || p_lower.contains("памят")
                || p_lower.contains("поток")
                || p_lower.contains("сервер")
                || p_lower.contains("клиент")
                || p_lower.contains("запрос")
                || p_lower.contains("баз")
                || p_lower.contains("алгоритм")
                || p_lower.contains("оптимиз")
                || p_lower.contains("расскажи")
                || p_lower.contains("объясни")
                || p_lower.contains("explain")
                || p_lower.contains("describe")
                || p_lower.contains("overview")
                || p_lower.contains("implement")
                || p_lower.contains("refactor")
                || p_lower.contains("fix")
                || p_lower.contains("debug")
                || p_lower.contains("error")
                || p_lower.contains("panic")
                || p_lower.contains("bug")
                || p_lower.contains("compile")
                || p_lower.contains("build")
                || p_lower.contains("test")
                || p_lower.contains("write")
                || p_lower.contains("modify")
                || p_lower.contains("server")
                || p_lower.contains("memory")
                || p_lower.contains("database")
                || p_lower.contains("borrow")
                || p_lower.contains("architecture")
                || p_lower.contains("algorithm")
                || p_lower.contains("optimize");

            // 6. Substantive / analytical question indicators
            let is_substantive_question = (prompt.contains('?')
                || p_lower.starts_with("как ")
                || p_lower.starts_with("почему ")
                || p_lower.starts_with("зачем ")
                || p_lower.starts_with("в чем ")
                || p_lower.starts_with("в чём ")
                || p_lower.starts_with("что такое ")
                || p_lower.starts_with("что лучше ")
                || p_lower.starts_with("что ")
                || p_lower.starts_with("какой ")
                || p_lower.starts_with("какая ")
                || p_lower.starts_with("какие ")
                || p_lower.starts_with("где ")
                || p_lower.starts_with("когда ")
                || p_lower.starts_with("сравни ")
                || p_lower.contains("разниц")
                || p_lower.starts_with("how ")
                || p_lower.starts_with("why ")
                || p_lower.starts_with("what is ")
                || p_lower.starts_with("what ")
                || p_lower.starts_with("which ")
                || p_lower.starts_with("where ")
                || p_lower.starts_with("when ")
                || p_lower.starts_with("compare ")
                || p_lower.contains("difference")
                || p_lower.contains(" vs ")
                || p_lower.contains("versus"))
                && !is_greeting;

            // 7. Code syntax indicators & technical identifiers
            let has_code_syntax = prompt.contains("```")
                || prompt.contains("fn ")
                || prompt.contains("struct ")
                || prompt.contains("enum ")
                || prompt.contains("trait ")
                || prompt.contains("class ")
                || prompt.contains("impl ")
                || prompt.contains("def ")
                || prompt.contains("let ")
                || prompt.contains("const ")
                || prompt.contains("pub ")
                || prompt.contains("import ")
                || prompt.contains("::")
                || prompt.contains("->")
                || prompt.contains("=>")
                || prompt.contains("&&")
                || prompt.contains("||")
                || prompt.contains("!=")
                || prompt.contains("==")
                || prompt.contains("&str")
                || prompt.contains("String")
                || prompt.contains("Option<")
                || prompt.contains("Result<")
                || prompt.contains("Vec<")
                || prompt.contains("HashMap<")
                || prompt.contains("Arc<")
                || prompt.contains("Mutex<")
                || prompt.contains(".rs")
                || prompt.contains(".py")
                || prompt.contains(".js")
                || prompt.contains(".ts")
                || prompt.contains(".toml")
                || prompt.contains(".json")
                || prompt.contains(".yaml")
                || prompt.contains(".yml")
                || prompt.contains(".sql")
                || prompt.contains(".sh")
                || p_lower.contains("cargo ")
                || p_lower.contains("git ")
                || p_lower.contains("npm ")
                || p_lower.contains("docker ")
                || p_lower.contains("rustc ")
                || p_lower.contains("bash ")
                || p_lower.contains("grep ")
                || p_lower.contains("curl ");

            if is_greeting && !has_code_syntax && !has_tech_keywords && !is_substantive_question {
                return TaskComplexity::Minimal;
            }

            // Short standalone acknowledgement or pleasantry without question or technical context
            if prompt.len() <= 50
                && !has_code_syntax
                && !has_tech_keywords
                && !is_substantive_question
                && !prompt.contains('{')
                && !prompt.contains('}')
                && !prompt.contains(';')
            {
                return TaskComplexity::Minimal;
            }

            if has_code_syntax || has_tech_keywords || prompt.len() > 100 {
                return TaskComplexity::High;
            }

            if is_substantive_question {
                return TaskComplexity::Medium;
            }

            TaskComplexity::Medium
        }
        _ => TaskComplexity::Medium,
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
        | "привет" | "приветик" | "приветствую" | "здравствуйте" | "здравствуй"
        | "ку" | "хай" | "хей" | "салам" | "салам алейкум" | "салют" | "здорово" | "здарова"
        | "йо" | "йоу" | "хеллоу" | "доброе утро" | "добрый день" | "добрый вечер" | "доброй ночи"
        | "доброго времени" | "доброго времени суток" | "пока" | "до свидания" | "до скорого"
        | "thanks" | "thank you" | "thx" | "cool" | "nice" | "great" | "awesome" | "cheers"
        | "спасибо" | "спасибки" | "благодарю" | "от души" | "сяп" | "спс" | "пасиб" | "пасибо"
        | "супер" | "отлично" | "кайф" | "круто" | "класс" | "молодец" | "красава"
        | "кто ты" | "как дела" | "как жизнь" | "как ты" | "как оно" | "что делаешь"
        | "что ты такое" | "что умеешь" | "че как" | "чё как" | "help" | "помощь" | "помоги"
        | "who are you" | "how are you" | "what can you do" | "what are you doing" | "what's up" | "whats up"
    );
    if is_exact {
        return true;
    }

    // Word tokenization (splits on any non-alphanumeric character, handling punctuation and
    // common keyboard layout typo tails like 'Ё' or 'ё' after '!' in 'Привет!Ё')
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
        "привет" | "приветик" | "приветствую" | "здравствуйте" | "здравствуй"
        | "хай" | "хей" | "салют" | "ку" | "салам" | "здарова" | "здорово"
        | "йо" | "йоу" | "хеллоу" | "hello" | "hi" | "hey" | "howdy" | "sup"
    );

    let is_greeting_phrase = first == "добрый" || first == "доброе" || first == "доброй" || first == "доброго"
        || first == "good";

    if is_greeting_head || is_greeting_phrase {
        // Single word greeting like "Привет!" or "Hi!"
        if words.len() == 1 {
            return true;
        }
        // Accidental single-character trailing typo (e.g. 'ё' from 'Привет!Ё' due to Shift+1 layout key proximity)
        if words.len() == 2 && words[1].chars().count() == 1 {
            return true;
        }
        // Conversational greeting if all subsequent words are pleasantry words
        let is_all_pleasantry = words[1..].iter().all(|w| {
            let wl = w.to_lowercase();
            matches!(
                wl.as_str(),
                "друг" | "бро" | "чел" | "агент" | "flashagent" | "всем" | "все"
                | "день" | "утро" | "вечер" | "ночи" | "суток" | "времени" | "алейкум"
                | "как" | "дела" | "жизнь" | "ты" | "оно" | "нового" | "что" | "делаешь"
                | "умеешь" | "можешь" | "чем" | "занимаешься" | "там"
                | "there" | "all" | "everyone" | "friend" | "bro" | "agent" | "today"
                | "how" | "are" | "you" | "is" | "it" | "going" | "up" | "doing"
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
    fn test_analyze_turn_complexity() {
        use crate::types::{ChatMessage, Role};

        // 1. Simple greeting
        let msg_hello = vec![ChatMessage::user("Привет!")];
        assert_eq!(analyze_turn_complexity(&msg_hello), TaskComplexity::Minimal);

        // 2. Casual acknowledgement
        let msg_ok = vec![ChatMessage::user("спасибо, ок")];
        assert_eq!(analyze_turn_complexity(&msg_ok), TaskComplexity::Minimal);

        // 3. Simple greeting with memory block
        let msg_mem = vec![ChatMessage::user("# Memory (automatically loaded)\n\n---\n\nHello")];
        assert_eq!(analyze_turn_complexity(&msg_mem), TaskComplexity::Minimal);

        // 4. Code editing request
        let msg_code = vec![ChatMessage::user("Напиши функцию парсинга JSON в Rust")];
        assert_eq!(analyze_turn_complexity(&msg_code), TaskComplexity::High);

        // 5. Tool execution error
        let msg_err = vec![ChatMessage {
            role: Role::Tool,
            content: "error: compilation failed with code 1\npanicked at main.rs:42".to_string(),
            reasoning: None,
            tool_call_id: Some("1".into()),
            tool_calls: vec![],
        }];
        assert_eq!(analyze_turn_complexity(&msg_err), TaskComplexity::High);

        // 6. Tool short success
        let msg_tool_ok = vec![ChatMessage {
            role: Role::Tool,
            content: "Applied edits to src/lib.rs successfully".to_string(),
            reasoning: None,
            tool_call_id: Some("2".into()),
            tool_calls: vec![],
        }];
        assert_eq!(analyze_turn_complexity(&msg_tool_ok), TaskComplexity::Low);

        // 7. Substantive technical question with '?'
        let msg_q_tech = vec![ChatMessage::user("Как устроен borrow checker в Rust?")];
        assert_eq!(analyze_turn_complexity(&msg_q_tech), TaskComplexity::High);

        // 8. General question without tech keywords
        let msg_q_gen = vec![ChatMessage::user("Что такое DNS?")];
        assert_eq!(analyze_turn_complexity(&msg_q_gen), TaskComplexity::Medium);

        // 9. User answering agent's question ("Да") in ongoing task
        let msg_continuation = vec![
            ChatMessage::user("Исправь ошибку компиляции"),
            ChatMessage::assistant("Найдена ошибка типов. Исправить её прямо сейчас?"),
            ChatMessage::user("Да"),
        ];
        assert_eq!(analyze_turn_complexity(&msg_continuation), TaskComplexity::High);

        // 10. User picking option "1" in ongoing task
        let msg_choice = vec![
            ChatMessage::user("Настрой БД"),
            ChatMessage::assistant("1) SQLite\n2) PostgreSQL\nКакой вариант выбрать?"),
            ChatMessage::user("1"),
        ];
        assert_eq!(analyze_turn_complexity(&msg_choice), TaskComplexity::High);

        // 11. User explicit brevity override
        let msg_brevity = vec![ChatMessage::user("Назови столицу Франции, ответь одной строкой без рассуждений")];
        assert_eq!(analyze_turn_complexity(&msg_brevity), TaskComplexity::Minimal);

        // 12. User explicit deep reasoning override
        let msg_deep = vec![ChatMessage::user("Подумай пошагово и сравни B-деревья и LSM-деревья")];
        assert_eq!(analyze_turn_complexity(&msg_deep), TaskComplexity::High);

        // 13. Slang greeting
        let msg_slang = vec![ChatMessage::user("Салам алейкум!")];
        assert_eq!(analyze_turn_complexity(&msg_slang), TaskComplexity::Minimal);

        // 14. Typo / keyboard layout trailing artifact greetings
        assert_eq!(analyze_turn_complexity(&[ChatMessage::user("Привет!Ё")]), TaskComplexity::Minimal);
        assert_eq!(analyze_turn_complexity(&[ChatMessage::user("Привет!ё")]), TaskComplexity::Minimal);
        assert_eq!(analyze_turn_complexity(&[ChatMessage::user("привет)))")]), TaskComplexity::Minimal);
        assert_eq!(analyze_turn_complexity(&[ChatMessage::user("Хай!")]), TaskComplexity::Minimal);
        assert_eq!(analyze_turn_complexity(&[ChatMessage::user("ку!")]), TaskComplexity::Minimal);
        assert_eq!(analyze_turn_complexity(&[ChatMessage::user("Ку")]), TaskComplexity::Minimal);
        assert_eq!(analyze_turn_complexity(&[ChatMessage::user("Доброго времени суток")]), TaskComplexity::Minimal);
        assert_eq!(analyze_turn_complexity(&[ChatMessage::user("Привет! Чем занимаешься?")]), TaskComplexity::Minimal);
    }

    #[test]
    fn test_is_greeting_text_variants() {
        assert!(is_greeting_text("Привет!Ё"));
        assert!(is_greeting_text("Привет!ё"));
        assert!(is_greeting_text("Привет!"));
        assert!(is_greeting_text("привет)))"));
        assert!(is_greeting_text("Хай!"));
        assert!(is_greeting_text("ку"));
        assert!(is_greeting_text("Йо"));
        assert!(is_greeting_text("Салют!"));
        assert!(is_greeting_text("Доброго времени суток"));
        assert!(is_greeting_text("Hello there"));
        assert!(is_greeting_text("Hi!"));
        assert!(is_greeting_text("Спасибо"));
        assert!(is_greeting_text("Благодарю!"));
        assert!(is_greeting_text("Кто ты?"));
        assert!(is_greeting_text("Как дела?"));

        // Must not classify coding tasks as pure greetings
        assert!(!is_greeting_text("Привет, напиши мне веб-сервер на Rust"));
        assert!(!is_greeting_text("fn main() { println!(); }"));
        assert!(!is_greeting_text("Исправь баг в коде"));
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

        let hello = vec![ChatMessage::user("Привет")];
        assert_eq!(binary_prof.resolve_dynamic(&hello), Some("off"));

        let code = vec![ChatMessage::user("Исправь ошибку компиляции в main.rs")];
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
