//! OpenAI-compatible HTTP backend (LM Studio, Ollama's OpenAI shim, vLLM,
//! OpenRouter...). Thin transport over [`crate::parse`] primitives.

use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::parse::{ChunkParser, SseDecoder};
use crate::types::{ChatMessage, FinishReason, LlmError, LlmEvent, Role, ToolSpec};

/// OpenAI-compatible chat-completions backend.
pub struct OpenAiCompat {
    base_url: String,
    api_key: Option<String>,
    model: std::sync::Arc<std::sync::RwLock<String>>,
    client: reqwest::Client,
    profile: std::sync::Arc<std::sync::RwLock<Option<crate::thinking::ThinkingProfile>>>,
    discovery: std::sync::Arc<std::sync::RwLock<Option<crate::thinking::ServerDiscovery>>>,
}

impl OpenAiCompat {
    /// Create a backend pointed at e.g. `http://localhost:1234/v1`.
    pub fn new(base_url: impl Into<String>, model: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key,
            model: std::sync::Arc::new(std::sync::RwLock::new(model.into())),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(600))
                .build()
                .unwrap_or_default(),
            profile: std::sync::Arc::new(std::sync::RwLock::new(None)),
            discovery: std::sync::Arc::new(std::sync::RwLock::new(None)),
        }
    }

    /// Return the currently active model ID.
    pub fn model(&self) -> String {
        self.model.read().map(|m| m.clone()).unwrap_or_default()
    }

    /// Switch or set the active model ID.
    pub fn set_model(&self, model: impl Into<String>) {
        if let Ok(mut lock) = self.model.write() {
            *lock = model.into();
        }
    }

    /// Access discovered server state if discovery has run.
    pub fn discovery(&self) -> Option<crate::thinking::ServerDiscovery> {
        self.discovery.read().ok().and_then(|d| d.clone())
    }

    /// Pre-seed or explicitly set the thinking profile.
    pub fn with_profile(mut self, profile: crate::thinking::ThinkingProfile) -> Self {
        self.profile = std::sync::Arc::new(std::sync::RwLock::new(Some(profile)));
        self
    }

    /// Access the active thinking profile learned for this model.
    pub fn profile(&self) -> Option<crate::thinking::ThinkingProfile> {
        self.profile.read().ok().and_then(|p| p.clone())
    }

    /// Discover available models, loaded instances, context windows, and
    /// reasoning capabilities across LM Studio `/api/v1/models`, `/api/v0/models`,
    /// or standard `/v1/models`.
    pub async fn discover_server(&self) -> Option<crate::thinking::ServerDiscovery> {
        let root = self.base_url.strip_suffix("/v1").unwrap_or(&self.base_url);
        let urls_to_try = [
            format!("{root}/api/v1/models"),
            format!("{root}/api/v0/models"),
            format!("{}/models", self.base_url),
            format!("{root}/models"),
        ];

        for url in urls_to_try {
            let mut req = self.client.get(&url).timeout(std::time::Duration::from_secs(2));
            if let Some(key) = &self.api_key {
                req = req.bearer_auth(key);
            }
            if let Ok(resp) = req.send().await {
                if resp.status().is_success() {
                    if let Ok(val) = resp.json::<serde_json::Value>().await {
                        let models = crate::thinking::parse_server_models(&val);
                        if !models.is_empty() {
                            let current_model = self.model();
                            // Pick active model:
                            // 1. Current model if it matches an entry and is loaded in server memory
                            // 2. Any model that is actively loaded in server memory
                            // 3. Current model if it matches an entry (for servers not reporting loaded state)
                            // 4. Otherwise first model in the list
                            let active_opt = models
                                .iter()
                                .find(|m| m.is_loaded && !current_model.is_empty() && (m.id == current_model || m.id.contains(&current_model) || current_model.contains(&m.id)))
                                .cloned()
                                .or_else(|| models.iter().find(|m| m.is_loaded).cloned())
                                .or_else(|| models.iter().find(|m| !current_model.is_empty() && (m.id == current_model || m.id.contains(&current_model) || current_model.contains(&m.id))).cloned())
                                .or_else(|| models.first().cloned());

                            if let Some(ref active) = active_opt {
                                self.set_model(&active.id);
                                if active.thinking.supported {
                                    if let Ok(mut lock) = self.profile.write() {
                                        *lock = Some(active.thinking.clone());
                                    }
                                }
                            }

                            let disc = crate::thinking::ServerDiscovery {
                                base_url: self.base_url.clone(),
                                models,
                                active_model: active_opt,
                            };

                            if let Ok(mut lock) = self.discovery.write() {
                                *lock = Some(disc.clone());
                            }

                            return Some(disc);
                        }
                    }
                }
            }
        }
        None
    }

    /// Query the API to discover model capabilities and presets.
    pub async fn fetch_profile(&self) -> Option<crate::thinking::ThinkingProfile> {
        self.discover_server().await;
        self.profile()
    }

    fn body(&self, messages: &[ChatMessage], tools: &[ToolSpec], options: &crate::types::TurnOptions) -> serde_json::Value {
        let current_model = self.model();

        let current_profile = self.profile.read().ok().and_then(|p| p.clone()).unwrap_or_default();
        let resolved_effort: Option<String> = if let Some(effort_str) = &options.custom_effort {
            Some(effort_str.clone())
        } else if options.thinking == crate::types::ThinkingEffort::Off {
            current_profile.resolve_effort(options.thinking).map(String::from).or(Some("off".to_string()))
        } else if options.thinking == crate::types::ThinkingEffort::Auto {
            current_profile.resolve_dynamic(messages).map(String::from)
        } else if options.thinking != crate::types::ThinkingEffort::Default {
            current_profile.resolve_effort(options.thinking).map(String::from)
        } else {
            current_profile.default_preset.clone()
        };

        let is_local_or_lmstudio = self.base_url.contains("localhost")
            || self.base_url.contains("127.0.0.1")
            || self.base_url.contains("0.0.0.0")
            || current_profile.protocol == crate::thinking::ThinkingProtocol::LmStudio
            || current_profile.protocol == crate::thinking::ThinkingProtocol::BooleanFlag;

        let is_effort_off = resolved_effort
            .as_deref()
            .map(|e| e == "off" || e == "disabled" || e == "none" || e == "false" || e == "0")
            .unwrap_or(false);

        let mut has_system = false;
        let mut msgs: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| match m.role {
                Role::Tool => serde_json::json!({
                    "role": "tool",
                    "tool_call_id": m.tool_call_id,
                    "content": m.content,
                }),
                Role::Assistant => {
                    let mut v = serde_json::json!({ "role": "assistant", "content": m.content });
                    if let Some(ref r) = m.reasoning {
                        if !r.trim().is_empty() {
                            v["reasoning_content"] = serde_json::json!(r);
                        }
                    }
                    if !m.tool_calls.is_empty() {
                        v["tool_calls"] = serde_json::Value::Array(
                            m.tool_calls
                                .iter()
                                .map(|c| {
                                    serde_json::json!({
                                        "id": c.id,
                                        "type": "function",
                                        "function": {
                                            "name": c.name,
                                            "arguments": c.args_json,
                                        },
                                    })
                                })
                                .collect(),
                        );
                    }
                    v
                }
                Role::System => {
                    has_system = true;
                    let content = if is_effort_off {
                        if let Some(idx) = m.content.find("REASONING INSTRUCTIONS:") {
                            let (pre, post) = m.content.split_at(idx);
                            let post_rest = if let Some(task_idx) = post.find("TASK EXECUTION") {
                                &post[task_idx..]
                            } else {
                                ""
                            };
                            format!("{pre}<|think_off|>THINKING DISABLED:\n- Internal reasoning and thinking are turned off for this turn.\n- Do not generate any internal thinking, reasoning process, or stage headers.\n- Provide your direct response or tool calls immediately.\n\n{post_rest}")
                        } else if !m.content.contains("<|think_off|>") {
                            format!("<|think_off|>THINKING DISABLED:\n- Internal reasoning is disabled for this turn. Answer directly without thoughts or stages.\n\n{}", m.content)
                        } else {
                            m.content.clone()
                        }
                    } else if is_local_or_lmstudio {
                        if !m.content.contains("<|think_on|>") && !m.content.contains("<|think_off|>") {
                            format!("<|think_on|>{}", m.content)
                        } else {
                            m.content.clone()
                        }
                    } else {
                        m.content.clone()
                    };
                    serde_json::json!({ "role": "system", "content": content })
                }
                Role::User => serde_json::json!({ "role": "user", "content": m.content }),
            })
            .collect();

        if !has_system && is_local_or_lmstudio {
            msgs.insert(0, serde_json::json!({
                "role": "system",
                "content": "You are FlashAgent, a local coding assistant."
            }));
        }

        let mut body = serde_json::json!({
            "model": current_model,
            "messages": msgs,
            "stream": true,
            "stream_options": { "include_usage": true },
        });

        // Maximize prefix KV cache reuse (f_keep >= 0.9) on llama.cpp, LM Studio, vLLM and local endpoints
        if is_local_or_lmstudio {
            body["cache_prompt"] = serde_json::json!(true);
            body["prompt_cache"] = serde_json::json!(true);
        }

        if let Some(t) = options.temperature {
            body["temperature"] = serde_json::json!(t);
        }
        if let Some(p) = options.top_p {
            body["top_p"] = serde_json::json!(p);
        }
        if let Some(k) = options.top_k {
            body["top_k"] = serde_json::json!(k);
        }
        if let Some(rp) = options.repeat_penalty {
            body["repeat_penalty"] = serde_json::json!(rp);
            body["repetition_penalty"] = serde_json::json!(rp);
        }
        if let Some(pp) = options.presence_penalty {
            body["presence_penalty"] = serde_json::json!(pp);
        }
        if let Some(mp) = options.min_p {
            body["min_p"] = serde_json::json!(mp);
        }
        if let Some(mt) = options.max_tokens {
            body["max_tokens"] = serde_json::json!(mt);
        }

        if let Some(ref effort_str) = resolved_effort {
            let is_off = effort_str == "off" || effort_str == "disabled" || effort_str == "none" || effort_str == "false" || effort_str == "0";
            if current_profile.supported {
                current_profile.apply_to_request(&mut body, effort_str);
            } else if is_local_or_lmstudio && is_off {
                // For local / LM Studio endpoints without an explicit thinking profile,
                // proactively suppress thinking so models like Gemma 4 / Qwen don't spend limited token budgets on thinking.
                body["reasoning"] = serde_json::json!("off");
                body["enable_thinking"] = serde_json::json!(false);
                body["chat_template_kwargs"] = serde_json::json!({ "enable_thinking": false });
                body["chat_template_config"] = serde_json::json!({ "enable_thinking": false });
            }
        }

        if !tools.is_empty() {
            body["tools"] = serde_json::Value::Array(
                tools
                    .iter()
                    .map(|t| {
                        serde_json::json!({
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "description": t.description,
                                "parameters": serde_json::from_str::<serde_json::Value>(&t.parameters_json)
                                    .unwrap_or(serde_json::json!({})),
                            },
                        })
                    })
                    .collect(),
            );
        }
        body
    }
}

#[async_trait]
impl crate::LlmBackend for OpenAiCompat {
    fn name(&self) -> &str {
        "openai-compat"
    }

    async fn stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<BoxStream<'static, Result<LlmEvent, LlmError>>, LlmError> {
        self.stream_with_options(messages, tools, &crate::types::TurnOptions::default()).await
    }

    async fn stream_with_options(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        options: &crate::types::TurnOptions,
    ) -> Result<BoxStream<'static, Result<LlmEvent, LlmError>>, LlmError> {
        let mut req = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .json(&self.body(messages, tools, options));
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body_text = resp.text().await.unwrap_or_default();

            // On 400 Bad Request, adapt to supported presets from server feedback or fallback:
            if status == 400 {
                if let Some(learned_profile) = crate::thinking::ThinkingProfile::parse_api_error(&body_text) {
                    let has_presets = !learned_profile.presets.is_empty() && learned_profile.supported;
                    if let Ok(mut lock) = self.profile.write() {
                        *lock = Some(learned_profile.clone());
                    }
                    if has_presets {
                        // Immediately retry with learned profile!
                        return Box::pin(self.stream_with_options(messages, tools, options)).await;
                    } else {
                        // Server does not support thinking; retry without parameters
                        return Box::pin(self.stream(messages, tools)).await;
                    }
                }
                return Box::pin(self.stream(messages, tools)).await;
            }
            return Err(LlmError::Status { status, body: body_text });
        }

        let (tx, rx) = mpsc::channel::<Result<LlmEvent, LlmError>>(256);
        tokio::spawn(async move {
            let mut sse = SseDecoder::default();
            let mut parser = ChunkParser::default();
            let mut done_sent = false;
            let mut stream = resp.bytes_stream();
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(bytes) => {
                        for payload in sse.feed(&bytes) {
                            for ev in parser.feed(&payload) {
                                // Native tool-call deltas pass through; if the
                                // model emits tool calls as text instead, the
                                // scanner catches them when the caller feeds
                                // text through it (kept out of the wire loop
                                // here to avoid double-handling).
                                if matches!(ev, LlmEvent::Done(_)) {
                                    done_sent = true;
                                }
                                if tx.send(Ok(ev)).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(LlmError::Stream(e.to_string()))).await;
                        return;
                    }
                }
            }
            if !done_sent {
                let _ = tx.send(Ok(LlmEvent::Done(FinishReason::Stop))).await;
            }
        });

        Ok(Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_includes_tools_and_stream() {
        let b = OpenAiCompat::new("http://localhost:1234/v1", "test-model", None);
        let body = b.body(
            &[ChatMessage::user("hi")],
            &[ToolSpec {
                name: "shell".into(),
                description: "run".into(),
                parameters_json: r#"{"type":"object"}"#.into(),
            }],
            &crate::types::TurnOptions::default(),
        );
        assert_eq!(body["stream"], true);
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["tools"][0]["function"]["name"], "shell");
    }

    #[test]
    fn body_tool_result_shape() {
        let b = OpenAiCompat::new("http://x/v1", "m", None);
        let body = b.body(
            &[
                ChatMessage::assistant("calling"),
                ChatMessage::tool_result("c1", "out"),
            ],
            &[],
            &crate::types::TurnOptions::default(),
        );
        assert_eq!(body["messages"][1]["role"], "tool");
        assert_eq!(body["messages"][1]["tool_call_id"], "c1");
    }

    #[test]
    fn body_includes_thinking_effort_when_specified() {
        let b = OpenAiCompat::new("http://localhost:1234/v1", "test-model", None);
        let body_low = b.body(
            &[ChatMessage::user("hi")],
            &[],
            &crate::types::TurnOptions { thinking: crate::types::ThinkingEffort::Low, ..Default::default() },
        );
        assert_eq!(body_low["reasoning_effort"], "low");

        // Custom binary profile (off/on)
        let b_binary = OpenAiCompat::new("http://localhost:1234/v1", "binary-model", None)
            .with_profile(crate::thinking::ThinkingProfile {
                presets: vec!["off".into(), "on".into()],
                protocol: crate::thinking::ThinkingProtocol::BooleanFlag,
                supported: true,
                default_preset: None,
            });
        let body_binary = b_binary.body(
            &[ChatMessage::user("hi")],
            &[],
            &crate::types::TurnOptions { thinking: crate::types::ThinkingEffort::Off, ..Default::default() },
        );
        assert_eq!(body_binary["enable_thinking"], false);

        // Custom LM Studio profile (off/on) with default on
        let b_lm = OpenAiCompat::new("http://localhost:1234/v1", "gemma-4", None)
            .with_profile(crate::thinking::ThinkingProfile {
                presets: vec!["off".into(), "on".into()],
                protocol: crate::thinking::ThinkingProtocol::LmStudio,
                supported: true,
                default_preset: Some("on".into()),
            });
        // Default effort explicitly requests default preset "on"
        let body_lm_default = b_lm.body(
            &[ChatMessage::user("hi")],
            &[],
            &crate::types::TurnOptions { thinking: crate::types::ThinkingEffort::Default, ..Default::default() },
        );
        assert_eq!(body_lm_default["reasoning"], "on");
        assert_eq!(body_lm_default["enable_thinking"], true);

        // Auto mode (TurnOptions::default()): for "Привет!", dynamically selects "off"
        let sys_template = "You are FlashAgent. REASONING INSTRUCTIONS:\n- break down into bold stages\n\nTASK EXECUTION:\n- write clean code";
        let body_lm_auto_hi = b_lm.body(
            &[
                ChatMessage::system(sys_template),
                ChatMessage::user("Привет!"),
            ],
            &[],
            &crate::types::TurnOptions::default(),
        );
        assert_eq!(body_lm_auto_hi["reasoning"], "off");
        assert_eq!(body_lm_auto_hi["enable_thinking"], false);
        assert_eq!(body_lm_auto_hi["chat_template_kwargs"]["enable_thinking"], false);
        let sys_hi = body_lm_auto_hi["messages"][0]["content"].as_str().unwrap();
        assert!(sys_hi.contains("<|think_off|>THINKING DISABLED:"));
        assert!(!sys_hi.contains("REASONING INSTRUCTIONS:"));

        // Auto mode for coding task: dynamically selects "on"
        let body_lm_auto_code = b_lm.body(
            &[
                ChatMessage::system(sys_template),
                ChatMessage::user("Напиши функцию парсинга на Rust"),
            ],
            &[],
            &crate::types::TurnOptions::default(),
        );
        assert_eq!(body_lm_auto_code["reasoning"], "on");
        assert_eq!(body_lm_auto_code["enable_thinking"], true);
        assert_eq!(body_lm_auto_code["chat_template_kwargs"]["enable_thinking"], true);
        let sys_code = body_lm_auto_code["messages"][0]["content"].as_str().unwrap();
        assert!(sys_code.contains("<|think_on|>"));
        assert!(sys_code.contains("REASONING INSTRUCTIONS:"));

        let body_lm_off = b_lm.body(
            &[ChatMessage::user("hi")],
            &[],
            &crate::types::TurnOptions { thinking: crate::types::ThinkingEffort::Off, ..Default::default() },
        );
        assert_eq!(body_lm_off["reasoning"], "off");
        assert_eq!(body_lm_off["enable_thinking"], false);

        // Custom xhigh profile (low/high/xhigh)
        let b_xhigh = OpenAiCompat::new("http://localhost:1234/v1", "xhigh-model", None)
            .with_profile(crate::thinking::ThinkingProfile {
                presets: vec!["low".into(), "high".into(), "xhigh".into()],
                protocol: crate::thinking::ThinkingProtocol::ReasoningEffort,
                supported: true,
                default_preset: None,
            });
        let body_min = b_xhigh.body(
            &[ChatMessage::user("hi")],
            &[],
            &crate::types::TurnOptions { thinking: crate::types::ThinkingEffort::Off, ..Default::default() },
        );
        assert_eq!(body_min["reasoning_effort"], "low");
        let body_max = b_xhigh.body(
            &[ChatMessage::user("hi")],
            &[],
            &crate::types::TurnOptions { thinking: crate::types::ThinkingEffort::High, ..Default::default() },
        );
        assert_eq!(body_max["reasoning_effort"], "xhigh");

        // Test explicit custom_effort override
        let body_custom = b_xhigh.body(
            &[ChatMessage::user("hi")],
            &[],
            &crate::types::TurnOptions {
                custom_effort: Some("xhigh".into()),
                ..Default::default()
            },
        );
        assert_eq!(body_custom["reasoning_effort"], "xhigh");
    }
}

