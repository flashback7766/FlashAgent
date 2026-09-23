//! OpenAI-compatible HTTP backend (LM Studio, Ollama, vLLM, OpenRouter...).

use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::parse::{ChunkParser, SseDecoder};
use crate::types::{ChatMessage, FinishReason, LlmError, LlmEvent, Role, ToolSpec};

pub struct OpenAiCompat {
    base_url: String,
    api_key: Option<String>,
    model: std::sync::Arc<std::sync::RwLock<String>>,
    client: reqwest::Client,
    profile: std::sync::Arc<std::sync::RwLock<Option<crate::thinking::ThinkingProfile>>>,
    discovery: std::sync::Arc<std::sync::RwLock<Option<crate::thinking::ServerDiscovery>>>,
    working_models_url: std::sync::Arc<std::sync::RwLock<Option<String>>>,
    /// Retries after a connection failure only. HTTP errors and mid-stream drops
    /// are not retried: the request may already have had effects.
    max_retries: std::sync::atomic::AtomicUsize,
    /// Auto effort shift in presets, learned from this model's past turns.
    effort_bias: std::sync::Arc<std::sync::atomic::AtomicI8>,
    /// Model requests not yet finished, streams included. Background polling
    /// (the model list) waits for zero instead of competing with them.
    in_flight: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// What this server rejected, kept for the model: without it every turn
    /// on a strict cloud API paid for a refused request first.
    learned: std::sync::Arc<std::sync::RwLock<Learned>>,
}

/// Request fields that are not in every OpenAI-compatible API. A server that
/// names one in a 400/422 gets requests without it from then on.
const OPTIONAL_FIELDS: [&str; 10] = [
    "temperature", "top_p", "top_k", "repeat_penalty", "repetition_penalty", "presence_penalty", "min_p",
    "stream_options", "cache_prompt", "prompt_cache",
];

#[derive(Debug, Clone, Default, PartialEq)]
struct Learned {
    fields: Option<Fields>,
    dropped: Vec<&'static str>,
    /// Newer OpenAI models take `max_completion_tokens` and refuse `max_tokens`.
    completion_tokens: bool,
}

impl Learned {
    /// From the server's error text; false when it named nothing we can drop.
    fn learn(&mut self, error: &str) -> bool {
        let error = error.to_lowercase();
        if !self.completion_tokens && error.contains("max_completion_tokens") {
            self.completion_tokens = true;
            return true;
        }
        let named: Vec<&'static str> = OPTIONAL_FIELDS
            .iter()
            .copied()
            .filter(|f| !self.dropped.contains(f) && mentions(&error, f))
            .collect();
        self.dropped.extend(&named);
        !named.is_empty()
    }

    fn apply(&self, body: &mut serde_json::Value) {
        let Some(map) = body.as_object_mut() else { return };
        for field in &self.dropped {
            map.remove(*field);
        }
        if self.completion_tokens {
            if let Some(n) = map.remove("max_tokens") {
                map.insert("max_completion_tokens".into(), n);
            }
        }
    }
}

/// `top_p` is named in "top_p is not supported", not in "top_probs".
fn mentions(text: &str, field: &str) -> bool {
    text.match_indices(field).any(|(i, _)| {
        let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
        !text[..i].chars().next_back().is_some_and(word) && !text[i + field.len()..].chars().next().is_some_and(word)
    })
}

struct Busy(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl Busy {
    fn new(counter: &std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Self {
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Busy(counter.clone())
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

const MAX_ADAPTIVE_RETRIES: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fields {
    All,
    /// Thinking controls, but only temperature and max_tokens for sampling.
    NoExtraSampling,
    Standard,
}

impl Fields {
    fn fewer(self) -> Self {
        match self {
            Fields::All => Fields::NoExtraSampling,
            _ => Fields::Standard,
        }
    }
}

impl OpenAiCompat {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key,
            model: std::sync::Arc::new(std::sync::RwLock::new(model.into())),
            // No total timeout: a slow local model can stream for many minutes. The idle
            // read timeout catches a server that stopped sending.
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .read_timeout(Duration::from_secs(300))
                .build()
                .unwrap_or_default(),
            profile: std::sync::Arc::new(std::sync::RwLock::new(None)),
            discovery: std::sync::Arc::new(std::sync::RwLock::new(None)),
            working_models_url: std::sync::Arc::new(std::sync::RwLock::new(None)),
            max_retries: std::sync::atomic::AtomicUsize::new(0),
            effort_bias: std::sync::Arc::new(std::sync::atomic::AtomicI8::new(0)),
            in_flight: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            learned: std::sync::Arc::default(),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn requests_in_flight(&self) -> usize {
        self.in_flight.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn set_max_retries(&self, retries: usize) {
        self.max_retries.store(retries, std::sync::atomic::Ordering::Relaxed);
    }

    /// Only auto is affected; a preset the user picked by hand stays as is.
    pub fn set_effort_bias(&self, steps: i8) {
        self.effort_bias.store(steps.clamp(-1, 1), std::sync::atomic::Ordering::Relaxed);
    }

    pub fn effort_bias(&self) -> i8 {
        self.effort_bias.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn model(&self) -> String {
        self.model.read().map(|m| m.clone()).unwrap_or_default()
    }

    pub fn set_model(&self, model: impl Into<String>) {
        let model = model.into();
        let changed = match self.model.write() {
            Ok(mut lock) => {
                let changed = *lock != model;
                *lock = model.clone();
                changed
            }
            Err(_) => return,
        };
        if !changed {
            return;
        }

        // A thinking profile belongs to the model, so the old one is dropped;
        // otherwise a non-reasoning model gets asked for a reasoning effort.
        let derived = self.discovery.read().ok().and_then(|d| {
            d.as_ref().and_then(|disc| {
                disc.models
                    .iter()
                    .find(|m| m.id == model)
                    .map(|m| m.thinking.clone())
            })
        });
        if let Ok(mut lock) = self.profile.write() {
            *lock = derived;
        }
        if let Ok(mut lock) = self.learned.write() {
            *lock = Learned::default();
        }
    }

    pub fn discovery(&self) -> Option<crate::thinking::ServerDiscovery> {
        self.discovery.read().ok().and_then(|d| d.clone())
    }

    /// Also checks `active_model`: some servers do not list the loaded model in
    /// `models`, and without the fallback no profile was adopted.
    pub fn adopt_discovery(&self, disc: &crate::thinking::ServerDiscovery) {
        let model = self.model();
        let thinking = disc
            .models
            .iter()
            .find(|m| m.id == model)
            .or_else(|| disc.active_model.as_ref().filter(|m| m.id == model))
            .map(|m| m.thinking.clone());
        if let Ok(mut lock) = self.discovery.write() {
            *lock = Some(disc.clone());
        }
        if let Some(thinking) = thinking {
            if let Ok(mut lock) = self.profile.write() {
                *lock = Some(thinking);
            }
        }
    }

    fn server_kind(&self) -> crate::thinking::ServerKind {
        self.discovery.read().ok().and_then(|d| d.as_ref().map(|d| d.kind)).unwrap_or_default()
    }

    pub fn with_profile(mut self, profile: crate::thinking::ThinkingProfile) -> Self {
        self.profile = std::sync::Arc::new(std::sync::RwLock::new(Some(profile)));
        self
    }

    pub fn profile(&self) -> Option<crate::thinking::ThinkingProfile> {
        self.profile.read().ok().and_then(|p| p.clone())
    }

    async fn probe_candidate(
        client: &reqwest::Client,
        url: &str,
        api_key: Option<&str>,
    ) -> Option<serde_json::Value> {
        let mut req = client.get(url).timeout(std::time::Duration::from_millis(1500));
        if let Some(key) = api_key {
            req = req.bearer_auth(key);
        }
        if let Ok(resp) = req.send().await {
            if resp.status().is_success() {
                return resp.json::<serde_json::Value>().await.ok();
            }
        }
        None
    }

    fn apply_discovery(
        &self,
        url: &str,
        val: &serde_json::Value,
        extra_v0: Option<&serde_json::Value>,
    ) -> Option<crate::thinking::ServerDiscovery> {
        // OpenRouter's list is also at `/api/v1/models`; only LM Studio's answer
        // has a `models` array or a per-model load `state`.
        let lm_studio_shape = val.get("models").is_some_and(|m| m.is_array())
            || val["data"].as_array().is_some_and(|d| d.iter().any(|m| m.get("state").is_some() || m.get("loaded_instances").is_some()));
        let kind = if (url.ends_with("/api/v1/models") || url.ends_with("/api/v0/models")) && lm_studio_shape {
            crate::thinking::ServerKind::LmStudio
        } else if val["data"]
            .as_array()
            .is_some_and(|d| d.iter().any(|m| m["owned_by"] == "llamacpp"))
        {
            crate::thinking::ServerKind::LlamaCpp
        } else {
            crate::thinking::ServerKind::Other
        };

        let mut models = crate::thinking::parse_server_models(val);
        if let Some(extra) = extra_v0 {
            models = crate::thinking::merge_server_models(models, crate::thinking::parse_server_models(extra));
        }

        if models.is_empty() {
            return None;
        }

        if let Ok(mut lock) = self.working_models_url.write() {
            *lock = Some(url.to_string());
        }
        let current_model = self.model();
        // The exact name before a similar one: `gpt-4o` must not become
        // `gpt-4o-audio-preview` because the list happens to name that first.
        let exact = |m: &crate::thinking::DiscoveredModel| !current_model.is_empty() && m.id == current_model;
        let similar = |m: &crate::thinking::DiscoveredModel| {
            !current_model.is_empty() && (m.id.contains(&current_model) || current_model.contains(&m.id))
        };
        let active_opt = models
            .iter()
            .find(|m| m.is_loaded && exact(m))
            .or_else(|| models.iter().find(|m| m.is_loaded && similar(m)))
            .or_else(|| models.iter().find(|m| m.is_loaded))
            .or_else(|| models.iter().find(|m| exact(m)))
            .or_else(|| models.iter().find(|m| similar(m)))
            .or_else(|| models.first())
            .cloned();

        if let Some(ref active) = active_opt {
            self.set_model(&active.id);
            if let Ok(mut lock) = self.profile.write() {
                if active.thinking.supported || lock.is_none() {
                    *lock = Some(active.thinking.clone());
                }
            }
        }

        let disc = crate::thinking::ServerDiscovery {
            base_url: self.base_url.clone(),
            models,
            active_model: active_opt,
            kind,
        };

        if let Ok(mut lock) = self.discovery.write() {
            *lock = Some(disc.clone());
        }

        Some(disc)
    }

    /// Tries LM Studio `/api/v1/models`, `/api/v0/models` and standard `/v1/models`.
    pub async fn discover_server(&self) -> Option<crate::thinking::ServerDiscovery> {
        let root = self.base_url.strip_suffix("/v1").unwrap_or(&self.base_url);
        let cached_url = self.working_models_url.read().ok().and_then(|u| u.clone());

        if let Some(ref url) = cached_url {
            if let Some(val) = Self::probe_candidate(&self.client, url, self.api_key.as_deref()).await {
                let mut extra_v0 = None;
                let extra_holder;
                if url.ends_with("/api/v1/models") {
                    let v0_url = format!("{root}/api/v0/models");
                    extra_holder = Self::probe_candidate(&self.client, &v0_url, self.api_key.as_deref()).await;
                    extra_v0 = extra_holder.as_ref();
                }
                if let Some(disc) = self.apply_discovery(url, &val, extra_v0) {
                    return Some(disc);
                }
            }
            if let Ok(mut lock) = self.working_models_url.write() {
                *lock = None;
            }
        }

        let mut candidate_urls = Vec::with_capacity(4);
        let defaults = [
            format!("{root}/api/v1/models"),
            format!("{root}/api/v0/models"),
            format!("{}/models", self.base_url),
            format!("{root}/models"),
        ];
        for d in defaults {
            if !candidate_urls.contains(&d) {
                candidate_urls.push(d);
            }
        }

        let probe_futs: Vec<_> = candidate_urls
            .iter()
            .map(|u| {
                let u = u.clone();
                let client = self.client.clone();
                let key = self.api_key.clone();
                async move {
                    let val = Self::probe_candidate(&client, &u, key.as_deref()).await;
                    (u, val)
                }
            })
            .collect();
        let results = futures::future::join_all(probe_futs).await;

        let v0_url = format!("{root}/api/v0/models");
        let v0_cached = results
            .iter()
            .find(|(u, v)| u == &v0_url && v.is_some())
            .and_then(|(_, v)| v.as_ref());

        for (url, maybe_val) in &results {
            if let Some(val) = maybe_val {
                let extra_v0 = if url.ends_with("/api/v1/models") {
                    v0_cached
                } else {
                    None
                };
                if let Some(disc) = self.apply_discovery(url, val, extra_v0) {
                    return Some(disc);
                }
            }
        }

        None
    }

    #[cfg(test)]
    fn body(&self, messages: &[ChatMessage], tools: &[ToolSpec], options: &crate::types::TurnOptions) -> serde_json::Value {
        let learned = self.learned.read().map(|l| l.clone()).unwrap_or_default();
        self.body_at(messages, tools, options, Fields::All, &learned)
    }

    fn body_at(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        options: &crate::types::TurnOptions,
        fields: Fields,
        learned: &Learned,
    ) -> serde_json::Value {
        let current_model = self.model();

        // Unknown abilities default to nothing: a guessed preset puts fields in the
        // request that the server warns about.
        let current_profile = self.profile.read().ok().and_then(|p| p.clone()).unwrap_or_default();
        let resolved_effort: Option<String> = if let Some(effort_str) = &options.custom_effort {
            Some(effort_str.clone())
        } else if options.thinking == crate::types::ThinkingEffort::Off {
            current_profile.resolve_effort(options.thinking).map(String::from).or(Some("off".to_string()))
        } else if options.thinking == crate::types::ThinkingEffort::Auto {
            current_profile
                .resolve_dynamic_biased(messages, self.effort_bias())
                .map(String::from)
        } else if options.thinking != crate::types::ThinkingEffort::Default {
            current_profile.resolve_effort(options.thinking).map(String::from)
        } else {
            current_profile.default_preset.clone()
        };

        // Kind of server, not its address: a remote llama-server still caches
        // prompts, a hosted API tunnelled to localhost does not.
        let is_local_or_lmstudio = self.server_kind().runs_local_models()
            || current_profile.protocol == crate::thinking::ThinkingProtocol::LmStudio
            || current_profile.protocol == crate::thinking::ThinkingProtocol::BooleanFlag;

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
                    // Sent unchanged regardless of thinking: the system prompt is the cache
                    // prefix, and rewriting it made the server re-read the whole conversation.
                    // The per-turn instruction goes at the end instead.
                    serde_json::json!({ "role": "system", "content": m.content })
                }
                Role::User => {
                    if m.images.is_empty() {
                        serde_json::json!({ "role": "user", "content": m.content })
                    } else {
                        // Text first, so the question is read before the image.
                        let mut parts = Vec::new();
                        if !m.content.trim().is_empty() {
                            parts.push(serde_json::json!({ "type": "text", "text": m.content }));
                        }
                        for url in &m.images {
                            parts.push(serde_json::json!({
                                "type": "image_url",
                                "image_url": { "url": url }
                            }));
                        }
                        serde_json::json!({ "role": "user", "content": parts })
                    }
                }
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

        if let Some(t) = options.temperature {
            body["temperature"] = serde_json::json!(t);
        }
        if let Some(mt) = options.max_tokens {
            body["max_tokens"] = serde_json::json!(mt);
        }
        if fields == Fields::Standard {
            return self.finish(body, tools, learned);
        }

        // Keeps prefix KV cache reuse high (f_keep >= 0.9).
        if is_local_or_lmstudio {
            body["cache_prompt"] = serde_json::json!(true);
            body["prompt_cache"] = serde_json::json!(true);
        }

        if fields == Fields::All {
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
        }

        if let Some(ref effort_str) = resolved_effort {
            let is_off = effort_str == "off" || effort_str == "disabled" || effort_str == "none" || effort_str == "false" || effort_str == "0";
            if current_profile.supported {
                current_profile.apply_to_request(&mut body, effort_str);
            } else if (is_local_or_lmstudio || current_profile.is_unreported()) && is_off {
                // No explicit profile on a local server: turn thinking off so small models
                // do not spend their token budget on it.
                body["reasoning"] = serde_json::json!("off");
                body["reasoning_effort"] = serde_json::json!("none");
                body["enable_thinking"] = serde_json::json!(false);
                body["chat_template_kwargs"] = serde_json::json!({ "thinking": false, "enable_thinking": false });
                body["chat_template_config"] = serde_json::json!({ "thinking": false, "enable_thinking": false });
            }
        }

        self.finish(body, tools, learned)
    }

    fn finish(&self, mut body: serde_json::Value, tools: &[ToolSpec], learned: &Learned) -> serde_json::Value {
        learned.apply(&mut body);
        self.with_tools(body, tools)
    }

    fn with_tools(&self, mut body: serde_json::Value, tools: &[ToolSpec]) -> serde_json::Value {
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
        // A 400 often means an optional field was rejected. Up to
        // MAX_ADAPTIVE_RETRIES times: learn the preset list if the error names one,
        // else drop fields (All -> NoExtraSampling -> Standard).
        let busy = Busy::new(&self.in_flight);
        let mut learned = self.learned.read().map(|l| l.clone()).unwrap_or_default();
        let known = learned.clone();
        let mut fields = learned.fields.unwrap_or(Fields::All);
        let mut adaptive_retries = 0u8;
        let resp = loop {
            let resp = self.send_with_retries(&self.body_at(messages, tools, options, fields, &learned)).await?;
            if resp.status().is_success() {
                // Kept only once a request without those fields went through: a
                // context overflow names no field and must not strip every request.
                // Merged, not replaced: a request running alongside (a subagent)
                // may have learned something else meanwhile.
                if learned != known {
                    if let Ok(mut lock) = self.learned.write() {
                        for field in learned.dropped {
                            if !lock.dropped.contains(&field) {
                                lock.dropped.push(field);
                            }
                        }
                        lock.completion_tokens |= learned.completion_tokens;
                        if learned.fields.is_some() {
                            lock.fields = learned.fields;
                        }
                    }
                }
                break resp;
            }
            let status = resp.status().as_u16();
            let body_text = resp.text().await.unwrap_or_default();
            // 422: Mistral and other pydantic servers refuse unknown fields with it.
            if !matches!(status, 400 | 422) || adaptive_retries >= MAX_ADAPTIVE_RETRIES {
                return Err(LlmError::Status { status, body: body_text });
            }
            adaptive_retries += 1;
            match crate::thinking::ThinkingProfile::parse_api_error(&body_text) {
                Some(learned) if fields == Fields::All && Some(&learned) != self.profile().as_ref() => {
                    if let Ok(mut lock) = self.profile.write() {
                        *lock = Some(learned);
                    }
                }
                _ => {
                    if !learned.learn(&body_text) {
                        if fields == Fields::Standard {
                            return Err(LlmError::Status { status, body: body_text });
                        }
                        fields = fields.fewer();
                        learned.fields = Some(fields);
                    }
                }
            }
        };

        let (tx, rx) = mpsc::channel::<Result<LlmEvent, LlmError>>(256);
        tokio::spawn(async move {
            // The server keeps generating while this task reads.
            let _busy = busy;
            let mut sse = SseDecoder::default();
            let mut parser = ChunkParser::default();
            let mut done_sent = false;
            let mut stream = resp.bytes_stream();
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(bytes) => {
                        for payload in sse.feed(&bytes) {
                            // Failures after the 200 (context overflow, crash) arrive in-stream.
                            if let Some(msg) = stream_error(&payload) {
                                let _ = tx.send(Err(LlmError::Stream(msg))).await;
                                return;
                            }
                            for ev in parser.feed(&payload) {
                                // Tool calls emitted as text are caught by the caller's scanner, not here.
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

/// The 1×1 is the control for the 64×64.
const PROBE_TINY: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R', 0, 0, 0, 1,
    0, 0, 0, 1, 8, 2, 0, 0, 0, 0x90, 0x77, 0x53, 0xDE, 0, 0, 0, 12, b'I', b'D', b'A', b'T', 0x08,
    0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB0, 0, 0,
    0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82,
];

impl OpenAiCompat {
    /// Measured, not guessed: Qwen-VL charges by area, Gemma a flat rate per
    /// image. Two requests, with and without a picture, give the difference.
    /// Returns `(per_pixel, fixed)`.
    pub async fn measure_image_cost(&self, probe_png: &[u8], width: u32, height: u32) -> Option<(f32, f32)> {
        let _busy = Busy::new(&self.in_flight);
        let ask = |images: Vec<String>| {
            let mut msg = ChatMessage::user("x");
            msg.images = images;
            serde_json::json!({
                "model": self.model(),
                "messages": [ {
                    "role": "user",
                    "content": if msg.images.is_empty() {
                        serde_json::json!("x")
                    } else {
                        serde_json::json!([
                            { "type": "text", "text": "x" },
                            { "type": "image_url", "image_url": { "url": msg.images[0] } }
                        ])
                    }
                } ],
                "max_tokens": 1,
                "stream": false,
            })
        };

        let prompt_tokens = |v: &serde_json::Value| -> Option<f32> {
            v.get("usage")?.get("prompt_tokens")?.as_f64().map(|n| n as f32)
        };

        let data_url = |bytes: &[u8]| format!("data:image/png;base64,{}", crate::base64_encode(bytes));

        let plain: serde_json::Value = self.send_with_retries(&ask(Vec::new())).await.ok()?.json().await.ok()?;
        let with_image: serde_json::Value =
            self.send_with_retries(&ask(vec![data_url(probe_png)])).await.ok()?.json().await.ok()?;
        let with_tiny: serde_json::Value =
            self.send_with_retries(&ask(vec![data_url(PROBE_TINY)])).await.ok()?.json().await.ok()?;

        let base = prompt_tokens(&plain)?;
        let big = prompt_tokens(&with_image)? - base;
        let tiny = prompt_tokens(&with_tiny)? - base;
        if big <= 0.0 {
            return None;
        }
        // A flat-rate model lands on per_pixel ≈ 0 by itself.
        let pixels = (width as f32) * (height as f32);
        let per_pixel = ((big - tiny) / pixels).max(0.0);
        Some((per_pixel, tiny.max(0.0)))
    }

    async fn send_with_retries(&self, body: &serde_json::Value) -> Result<reqwest::Response, LlmError> {
        let retries = self.max_retries.load(std::sync::atomic::Ordering::Relaxed);
        let mut attempt = 0usize;
        loop {
            let mut req = self.client.post(format!("{}/chat/completions", self.base_url)).json(body);
            if let Some(key) = &self.api_key {
                req = req.bearer_auth(key);
            }
            match req.send().await {
                // Nothing was generated: rate limits and an overloaded gateway
                // are worth waiting for, as long as the server's wait is short.
                Ok(resp) if attempt < retries.max(2) && matches!(resp.status().as_u16(), 429 | 502 | 503 | 504 | 529) => {
                    attempt += 1;
                    let wait = resp
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse::<f64>().ok())
                        // Negative, NaN or infinite would panic in `from_secs_f64`.
                        .and_then(|s| Duration::try_from_secs_f64(s).ok())
                        .unwrap_or(Duration::from_secs(2u64.pow(attempt as u32)));
                    if wait > Duration::from_secs(30) {
                        return Ok(resp);
                    }
                    tokio::time::sleep(wait).await;
                }
                Ok(resp) => return Ok(resp),
                // Connect errors only: after connecting, the server may already be generating.
                Err(e) if attempt < retries && e.is_connect() => {
                    attempt += 1;
                    tokio::time::sleep(Duration::from_millis(400 * attempt as u64)).await;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

fn stream_error(payload: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(payload).ok()?;
    let err = v.get("error").filter(|e| !e.is_null())?;
    Some(match err {
        serde_json::Value::String(s) => s.clone(),
        other => other
            .get("message")
            .and_then(|m| m.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| other.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LlmBackend;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn a_message_with_a_picture_is_sent_as_content_parts() {
        let llm = OpenAiCompat::new("http://localhost:1234/v1", "vlm", None);
        let mut msg = ChatMessage::user("why does this frame look wrong?");
        msg.images.push("data:image/png;base64,AAAA".to_string());
        let body = llm.body(&[msg], &[], &crate::types::TurnOptions::default());
        let user = body["messages"].as_array().unwrap().iter().find(|m| m["role"] == "user").unwrap().clone();
        let parts = user["content"].as_array().expect("content is a list of parts");
        assert_eq!(parts[0]["type"], "text", "the question comes before the picture it is about");
        assert_eq!(parts[0]["text"], "why does this frame look wrong?");
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,AAAA");
    }

    #[test]
    fn an_image_only_message_has_no_text_part() {
        let llm = OpenAiCompat::new("http://localhost:1234/v1", "vlm", None);
        let mut msg = ChatMessage::user("");
        msg.images.push("data:image/png;base64,AAAA".to_string());
        let body = llm.body(&[msg], &[], &crate::types::TurnOptions::default());
        let user = body["messages"].as_array().unwrap().iter().find(|m| m["role"] == "user").unwrap().clone();
        let parts = user["content"].as_array().expect("content is a list of parts");
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["type"], "image_url");
        assert_eq!(parts[0]["image_url"]["url"], "data:image/png;base64,AAAA");
    }


    #[test]
    fn a_message_without_pictures_is_sent_exactly_as_before() {
        let llm = OpenAiCompat::new("http://localhost:1234/v1", "m", None);
        let options = crate::types::TurnOptions { thinking: crate::types::ThinkingEffort::Default, ..Default::default() };
        let body = llm.body(&[ChatMessage::user("hello")], &[], &options);
        let user = body["messages"].as_array().unwrap().iter().find(|m| m["role"] == "user").unwrap().clone();
        assert_eq!(user["content"], "hello");
    }

    #[test]
    fn switching_models_does_not_keep_the_old_model_s_thinking_profile() {
        // Regression: the previous model's profile was kept after a model switch.
        let llm = OpenAiCompat::new("http://127.0.0.1:1234/v1", "thinker", None).with_profile(
            crate::thinking::ThinkingProfile {
                presets: vec!["off".into(), "on".into()],
                protocol: crate::thinking::ThinkingProtocol::LmStudio,
                supported: true,
                default_preset: Some("on".into()),
            },
        );
        assert!(llm.profile().is_some_and(|p| p.supported));
        llm.set_model("a-model-that-cannot-reason");
        assert!(
            llm.profile().is_none(),
            "an unknown model inherits nothing from the one before it"
        );
        llm.set_model("a-model-that-cannot-reason");
        assert!(llm.profile().is_none(), "setting the same model again changes nothing");
    }

    #[test]
    fn the_learned_effort_correction_is_never_more_than_one_step() {
        // Clamped, so a bad streak cannot pin the model at "off".
        let llm = OpenAiCompat::new("http://localhost:1234/v1", "m", None);
        assert_eq!(llm.effort_bias(), 0, "nothing learned yet");
        llm.set_effort_bias(-7);
        assert_eq!(llm.effort_bias(), -1);
        llm.set_effort_bias(7);
        assert_eq!(llm.effort_bias(), 1);
        llm.set_effort_bias(0);
        assert_eq!(llm.effort_bias(), 0);
    }

    async fn canned_server(status: &'static str, body: &'static str) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hits2 = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                hits2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut buf = vec![0u8; 65536];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        (format!("http://{addr}/v1"), hits)
    }

    #[tokio::test]
    async fn a_stream_counts_as_a_request_until_its_last_byte() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 65536];
            let _ = sock.read(&mut buf).await;
            let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
            let _ = sock.write_all(head.as_bytes()).await;
            let _ = sock.write_all(b"data: {\"choices\":[{\"delta\":{\"content\":\"par\"}}]}\n\n").await;
            let _ = release_rx.await;
            let _ = sock.write_all(b"data: [DONE]\n\n").await;
        });
        let b = OpenAiCompat::new(format!("http://{addr}/v1"), "m", None);
        assert_eq!(b.requests_in_flight(), 0);
        let mut stream = b.stream(&[ChatMessage::user("hi")], &[]).await.unwrap();
        assert!(matches!(stream.next().await, Some(Ok(LlmEvent::TextDelta(_)))));
        assert_eq!(b.requests_in_flight(), 1, "the server is still sending");
        release_tx.send(()).unwrap();
        while stream.next().await.is_some() {}
        assert_eq!(b.requests_in_flight(), 0, "the stream has ended");
    }

    #[tokio::test]
    async fn a_request_the_server_refuses_is_not_counted_as_running() {
        let (url, _) = canned_server("500 Internal Server Error", "boom").await;
        let b = OpenAiCompat::new(url, "m", None);
        assert!(b.stream(&[ChatMessage::user("hi")], &[]).await.is_err());
        assert_eq!(b.requests_in_flight(), 0);
    }

    #[tokio::test]
    async fn a_look_at_the_server_that_says_nothing_keeps_what_its_errors_taught() {
        // A listing that does not mention reasoning must not erase a preset list
        // learned from a 400.
        let (url, _) = canned_server("200 OK", r#"{"data":[{"id":"m","state":"loaded"}]}"#).await;
        let learned = crate::thinking::ThinkingProfile {
            presets: vec!["low".into(), "high".into()],
            protocol: crate::thinking::ThinkingProtocol::ReasoningEffort,
            supported: true,
            default_preset: None,
        };
        let b = OpenAiCompat::new(url, "m", None).with_profile(learned.clone());
        let disc = b.discover_server().await.expect("the listing was read");
        assert!(disc.models[0].thinking.is_unreported(), "the listing says nothing about reasoning");
        assert_eq!(b.profile(), Some(learned), "a look that said nothing replaced what the server's error taught");

        let fresh = OpenAiCompat::new(b.base_url().to_string(), "m", None);
        fresh.discover_server().await.expect("the listing was read");
        assert!(fresh.profile().is_some_and(|p| p.is_unreported()));
    }

    #[tokio::test]
    async fn persistent_400_is_retried_a_bounded_number_of_times() {
        // "[3]" looks like a preset list to the parser; this used to recurse forever.
        let (url, hits) = canned_server("400 Bad Request", r#"{"error":"invalid messages[3].content"}"#).await;
        let b = OpenAiCompat::new(url, "m", None);
        let res = b.stream(&[ChatMessage::user("hi")], &[]).await;
        assert!(matches!(res, Err(LlmError::Status { status: 400, .. })));
        assert!(hits.load(std::sync::atomic::Ordering::SeqCst) <= 1 + MAX_ADAPTIVE_RETRIES as usize);
    }

    #[tokio::test]
    async fn unrelated_400_never_becomes_a_thinking_profile_and_fallback_drops_extras() {
        // Rejects any body carrying `top_k`, with a bracketed number in the error.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 65536];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let resp = if req.contains("top_k") {
                    let body = r#"{"error":"Unrecognized request argument: top_k (messages[3])"}"#;
                    format!("HTTP/1.1 400 Bad Request\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
                } else {
                    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n";
                    format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
                };
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        let b = OpenAiCompat::new(format!("http://{addr}/v1"), "m", None);
        let opts = crate::types::TurnOptions { top_k: Some(20), temperature: Some(0.5), ..Default::default() };
        let mut stream = b.stream_with_options(&[ChatMessage::user("hi")], &[], &opts).await.expect("recovers without top_k");
        let mut text = String::new();
        while let Some(Ok(ev)) = stream.next().await {
            if let LlmEvent::TextDelta(t) = ev {
                text.push_str(&t);
            }
        }
        assert_eq!(text, "ok");
        assert!(b.profile().is_none(), "learned a bogus profile: {:?}", b.profile());
    }

    #[tokio::test]
    async fn a_field_the_server_names_is_left_out_from_then_on() {
        // A strict cloud API: refuses `top_k` and `max_tokens` by name.
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let log = seen.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let mut buf = vec![0u8; 65536];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                log.lock().unwrap().push(req.clone());
                let resp = if req.contains("\"top_k\"") {
                    let body = r#"{"error":{"message":"Unrecognized request argument supplied: top_k"}}"#;
                    format!("HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
                } else if req.contains("\"max_tokens\"") {
                    let body = r#"{"error":{"message":"Unsupported parameter: 'max_tokens' is not supported with this model. Use 'max_completion_tokens' instead."}}"#;
                    format!("HTTP/1.1 422 Unprocessable Entity\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
                } else {
                    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n";
                    format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
                };
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        let b = OpenAiCompat::new(format!("http://{addr}/v1"), "m", None);
        let opts = crate::types::TurnOptions { top_k: Some(20), top_p: Some(0.9), max_tokens: Some(100), ..Default::default() };
        for _ in 0..2 {
            let mut stream = b.stream_with_options(&[ChatMessage::user("hi")], &[], &opts).await.expect("adapts to the server");
            while stream.next().await.is_some() {}
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 4, "two refusals on the first turn, none on the second");
        let last = seen.last().unwrap();
        assert!(last.contains("\"max_completion_tokens\":100") && last.contains("\"top_p\"") && !last.contains("\"top_k\""), "{last}");
    }

    #[tokio::test]
    async fn a_refusal_that_names_no_field_does_not_strip_later_requests() {
        let (url, _) = canned_server(
            "400 Bad Request",
            r#"{"error":{"message":"This model's maximum context length is 8192 tokens."}}"#,
        )
        .await;
        let b = OpenAiCompat::new(url, "m", None);
        let opts = crate::types::TurnOptions { top_k: Some(20), ..Default::default() };
        assert!(b.stream_with_options(&[ChatMessage::user("hi")], &[], &opts).await.is_err());
        let learned = b.learned.read().unwrap().clone();
        assert!(learned.fields.is_none() && learned.dropped.is_empty(), "{learned:?}");
        assert!(b.body(&[ChatMessage::user("hi")], &[], &opts).get("top_k").is_some());
    }

    #[test]
    fn discovery_keeps_the_model_named_exactly_and_knows_openrouter_from_lm_studio() {
        let b = OpenAiCompat::new("https://openrouter.ai/api/v1", "openai/gpt-4o", None);
        let listing = serde_json::json!({ "data": [
            { "id": "openai/gpt-4o-audio-preview", "context_length": 128000 },
            { "id": "openai/gpt-4o", "context_length": 128000 }
        ] });
        let disc = b.apply_discovery("https://openrouter.ai/api/v1/models", &listing, None).expect("discovered");
        assert_eq!(b.model(), "openai/gpt-4o");
        assert_eq!(disc.kind, crate::thinking::ServerKind::Other);
    }

    #[test]
    fn a_field_is_named_only_as_a_whole_word() {
        assert!(mentions("top_p is not supported", "top_p"));
        assert!(!mentions("unknown field top_probs", "top_p"));
        assert!(!mentions("bad logit_temperature", "temperature"));
    }

    #[tokio::test]
    async fn in_stream_error_payload_surfaces_as_error() {
        let (url, _) = canned_server(
            "200 OK",
            "data: {\"choices\":[{\"delta\":{\"content\":\"par\"}}]}\n\ndata: {\"error\":{\"message\":\"context length exceeded\"}}\n\n",
        )
        .await;
        let b = OpenAiCompat::new(url, "m", None);
        let mut stream = b.stream(&[ChatMessage::user("hi")], &[]).await.unwrap();
        let mut saw_err = None;
        while let Some(item) = stream.next().await {
            if let Err(e) = item {
                saw_err = Some(e.to_string());
            }
        }
        assert!(saw_err.is_some_and(|e| e.contains("context length exceeded")));
    }

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

        let b_lm = OpenAiCompat::new("http://localhost:1234/v1", "gemma-4", None)
            .with_profile(crate::thinking::ThinkingProfile {
                presets: vec!["off".into(), "on".into()],
                protocol: crate::thinking::ThinkingProtocol::LmStudio,
                supported: true,
                default_preset: Some("on".into()),
            });
        let body_lm_default = b_lm.body(
            &[ChatMessage::user("hi")],
            &[],
            &crate::types::TurnOptions { thinking: crate::types::ThinkingEffort::Default, ..Default::default() },
        );
        assert_eq!(body_lm_default["reasoning"], "on");
        assert_eq!(body_lm_default["enable_thinking"], true);

        let sys_template = "You are FlashAgent. REASONING INSTRUCTIONS:\n- break down into bold stages\n\nTASK EXECUTION:\n- write clean code";
        let body_lm_auto_hi = b_lm.body(
            &[
                ChatMessage::system(sys_template),
                ChatMessage::user("Hello!"),
            ],
            &[],
            &crate::types::TurnOptions::default(),
        );
        assert_eq!(body_lm_auto_hi["reasoning"], "off");
        assert_eq!(body_lm_auto_hi["enable_thinking"], false);
        assert_eq!(body_lm_auto_hi["chat_template_kwargs"]["enable_thinking"], false);
        let sys_hi = body_lm_auto_hi["messages"][0]["content"].as_str().unwrap();
        assert_eq!(sys_hi, sys_template, "the system prompt is never rewritten: it is the cached prefix");
        let user_hi = body_lm_auto_hi["messages"][1]["content"].as_str().unwrap();
        assert_eq!(user_hi, "Hello!", "user messages are never rewritten: keeps prefix KV cache valid");

        let body_lm_auto_code = b_lm.body(
            &[
                ChatMessage::system(sys_template),
                ChatMessage::user("Write a parser function in Rust"),
            ],
            &[],
            &crate::types::TurnOptions::default(),
        );
        assert_eq!(body_lm_auto_code["reasoning"], "on");
        assert_eq!(body_lm_auto_code["enable_thinking"], true);
        assert_eq!(body_lm_auto_code["chat_template_kwargs"]["enable_thinking"], true);
        let sys_code = body_lm_auto_code["messages"][0]["content"].as_str().unwrap();
        assert_eq!(sys_code, sys_hi, "thinking on or off, the prompt starts with the same bytes");
        assert_eq!(body_lm_auto_code["messages"][1]["content"], "Write a parser function in Rust");

        let body_later = b_lm.body(
            &[
                ChatMessage::system(sys_template),
                ChatMessage::user("Write a parser function in Rust"),
                ChatMessage::assistant("Done."),
                ChatMessage::user("thanks"),
            ],
            &[],
            &crate::types::TurnOptions { thinking: crate::types::ThinkingEffort::Off, ..Default::default() },
        );
        assert_eq!(body_later["messages"][0]["content"], sys_template);
        assert_eq!(body_later["messages"][1]["content"], "Write a parser function in Rust");
        assert_eq!(body_later["messages"][3]["content"], "thanks");

        let body_lm_off = b_lm.body(
            &[ChatMessage::user("hi")],
            &[],
            &crate::types::TurnOptions { thinking: crate::types::ThinkingEffort::Off, ..Default::default() },
        );
        assert_eq!(body_lm_off["reasoning"], "off");
        assert_eq!(body_lm_off["enable_thinking"], false);

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

