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
    working_models_url: std::sync::Arc<std::sync::RwLock<Option<String>>>,
    /// Extra attempts after a transport failure (connection refused/reset)
    /// before any response arrived. HTTP errors and mid-stream drops are not
    /// retried: the request may already have had effects on the server.
    max_retries: std::sync::atomic::AtomicUsize,
    /// Preset steps to shift auto effort by, learned from how this model's
    /// turns have gone. Shared, because the app sets it from outside the
    /// request path.
    effort_bias: std::sync::Arc<std::sync::atomic::AtomicI8>,
    /// Requests to the model that have not finished yet, streams included
    /// until their last byte. Anything that only watches the server (the
    /// model list) waits for this to be zero rather than compete with the
    /// work for a local server's attention.
    in_flight: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

/// One request to the model, counted in `in_flight` for as long as it lives.
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

/// How many times a 400 may teach us a new request shape before we give up.
const MAX_ADAPTIVE_RETRIES: u8 = 2;

/// Which optional request fields a body carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fields {
    /// Thinking controls and every sampling knob.
    All,
    /// Thinking controls, but only standard sampling (temperature, max_tokens).
    NoExtraSampling,
    /// Only fields every OpenAI-compatible server accepts.
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
    /// Create a backend pointed at e.g. `http://localhost:1234/v1`.
    pub fn new(base_url: impl Into<String>, model: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key,
            model: std::sync::Arc::new(std::sync::RwLock::new(model.into())),
            // No total timeout: a long generation on a slow local model can
            // legitimately stream for many minutes. An idle read timeout
            // catches a server that stopped sending instead.
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
        }
    }

    /// Base URL this backend talks to.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Requests to the model still running, a stream counting until it ends.
    pub fn requests_in_flight(&self) -> usize {
        self.in_flight.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Retry budget for transport failures (the `network_retries` setting).
    pub fn set_max_retries(&self, retries: usize) {
        self.max_retries.store(retries, std::sync::atomic::Ordering::Relaxed);
    }

    /// Shift auto effort by `steps` presets for this model, as learned from
    /// its past turns. Only auto is affected: a preset the user picked by
    /// hand is theirs, and second-guessing it would be a bug.
    pub fn set_effort_bias(&self, steps: i8) {
        self.effort_bias.store(steps.clamp(-1, 1), std::sync::atomic::Ordering::Relaxed);
    }

    /// The correction in force right now.
    pub fn effort_bias(&self) -> i8 {
        self.effort_bias.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Return the currently active model ID.
    pub fn model(&self) -> String {
        self.model.read().map(|m| m.clone()).unwrap_or_default()
    }

    /// Switch or set the active model ID.
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

        // A thinking profile belongs to a model, not to a connection. Keeping
        // the old one means asking a model that cannot reason for a reasoning
        // effort — the server warns, and the request carries fields the model
        // has no use for. What discovery already knows about the new model is
        // the answer; when it knows nothing, so do we.
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
    }

    /// Access discovered server state if discovery has run.
    pub fn discovery(&self) -> Option<crate::thinking::ServerDiscovery> {
        self.discovery.read().ok().and_then(|d| d.clone())
    }

    /// Take what another backend's look at this server found, so this one
    /// knows the model's reasoning settings and the kind of server from its
    /// first request instead of from its own first look.
    ///
    /// Checks `active_model` as well as `models`: a server's listing does not
    /// always repeat the loaded model's entry there, and without this fallback
    /// adopting discovery for exactly that model silently kept no profile.
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

    /// What kind of server this is, as far as discovery knows.
    fn server_kind(&self) -> crate::thinking::ServerKind {
        self.discovery.read().ok().and_then(|d| d.as_ref().map(|d| d.kind)).unwrap_or_default()
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
        let kind = if url.ends_with("/api/v1/models") || url.ends_with("/api/v0/models") {
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
        let active_opt = models
            .iter()
            .find(|m| m.is_loaded && !current_model.is_empty() && (m.id == current_model || m.id.contains(&current_model) || current_model.contains(&m.id)))
            .cloned()
            .or_else(|| models.iter().find(|m| m.is_loaded).cloned())
            .or_else(|| models.iter().find(|m| !current_model.is_empty() && (m.id == current_model || m.id.contains(&current_model) || current_model.contains(&m.id))).cloned())
            .or_else(|| models.first().cloned());

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

    /// Discover available models, loaded instances, context windows, and
    /// reasoning capabilities across LM Studio `/api/v1/models`, `/api/v0/models`,
    /// or standard `/v1/models`.
    pub async fn discover_server(&self) -> Option<crate::thinking::ServerDiscovery> {
        let root = self.base_url.strip_suffix("/v1").unwrap_or(&self.base_url);
        let cached_url = self.working_models_url.read().ok().and_then(|u| u.clone());

        // Fast path: if a previous look already found a working endpoint, try it first
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

        // Candidates to probe concurrently, ordered by priority
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

        // Probe candidate URLs concurrently to prevent sequential timeout stalls
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

    /// Query the API to discover model capabilities and presets.
    pub async fn fetch_profile(&self) -> Option<crate::thinking::ThinkingProfile> {
        self.discover_server().await;
        self.profile()
    }

    #[cfg(test)]
    fn body(&self, messages: &[ChatMessage], tools: &[ToolSpec], options: &crate::types::TurnOptions) -> serde_json::Value {
        self.body_at(messages, tools, options, Fields::All)
    }

    fn body_at(&self, messages: &[ChatMessage], tools: &[ToolSpec], options: &crate::types::TurnOptions, fields: Fields) -> serde_json::Value {
        let current_model = self.model();

        // No discovered profile means the model's abilities are unknown, not
        // that it has every preset: the default is a guess, and a wrong guess
        // here puts fields in the request that the server logs warnings about.
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

        // What the server is, not where it is: a llama-server on another
        // machine keeps a prompt cache, and a hosted API on localhost through
        // a tunnel does not.
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
                    // Sent exactly as it is, whatever the thinking setting.
                    // It is the start of every prompt: rewriting it when
                    // thinking flipped between turns made the server's cache
                    // miss from the first token and re-read the whole
                    // conversation (f_keep near 0, tens of seconds on a local
                    // model). The per-turn instruction goes at the end instead.
                    serde_json::json!({ "role": "system", "content": m.content })
                }
                Role::User => {
                    if m.images.is_empty() {
                        serde_json::json!({ "role": "user", "content": m.content })
                    } else {
                        // A message with pictures is sent the way every
                        // OpenAI-compatible server expects them: content
                        // becomes a list of parts, text first so the question
                        // is read before the image it is about.
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
            return self.with_tools(body, tools);
        }

        // Maximize prefix KV cache reuse (f_keep >= 0.9) on llama.cpp, LM Studio, vLLM and local endpoints
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
                // For local / LM Studio endpoints without an explicit thinking profile,
                // proactively suppress thinking so models like Gemma 4 / Qwen don't spend limited token budgets on thinking.
                body["reasoning"] = serde_json::json!("off");
                body["reasoning_effort"] = serde_json::json!("none");
                body["enable_thinking"] = serde_json::json!(false);
                body["chat_template_kwargs"] = serde_json::json!({ "thinking": false, "enable_thinking": false });
                body["chat_template_config"] = serde_json::json!({ "thinking": false, "enable_thinking": false });
            }
        }

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
        // A 400 often means the server rejected an optional field. Adapt at
        // most MAX_ADAPTIVE_RETRIES times: learn a thinking preset list when
        // the error names one, otherwise fall back to fewer fields (all ->
        // no extra sampling knobs -> standard fields only). A 400 for any
        // other reason (context overflow, malformed history) then surfaces.
        let busy = Busy::new(&self.in_flight);
        let mut fields = Fields::All;
        let mut adaptive_retries = 0u8;
        let resp = loop {
            let resp = self.send_with_retries(&self.body_at(messages, tools, options, fields)).await?;
            if resp.status().is_success() {
                break resp;
            }
            let status = resp.status().as_u16();
            let body_text = resp.text().await.unwrap_or_default();
            if status != 400 || adaptive_retries >= MAX_ADAPTIVE_RETRIES {
                return Err(LlmError::Status { status, body: body_text });
            }
            adaptive_retries += 1;
            match crate::thinking::ThinkingProfile::parse_api_error(&body_text) {
                Some(learned) if fields == Fields::All && Some(&learned) != self.profile().as_ref() => {
                    if let Ok(mut lock) = self.profile.write() {
                        *lock = Some(learned);
                    }
                }
                _ => fields = fields.fewer(),
            }
        };

        let (tx, rx) = mpsc::channel::<Result<LlmEvent, LlmError>>(256);
        tokio::spawn(async move {
            // The request is not over when the headers arrive: the server is
            // still generating for as long as this task reads.
            let _busy = busy;
            let mut sse = SseDecoder::default();
            let mut parser = ChunkParser::default();
            let mut done_sent = false;
            let mut stream = resp.bytes_stream();
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(bytes) => {
                        for payload in sse.feed(&bytes) {
                            // Servers report failures that happen after the
                            // 200 (context overflow, model crash) as an
                            // `error` object inside the stream.
                            if let Some(msg) = stream_error(&payload) {
                                let _ = tx.send(Err(LlmError::Stream(msg))).await;
                                return;
                            }
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

/// A 1×1 PNG and a 64×64 PNG, used to ask the server what a picture costs.
/// Both are valid files; the small one is the control.
const PROBE_TINY: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R', 0, 0, 0, 1,
    0, 0, 0, 1, 8, 2, 0, 0, 0, 0x90, 0x77, 0x53, 0xDE, 0, 0, 0, 12, b'I', b'D', b'A', b'T', 0x08,
    0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB0, 0, 0,
    0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82,
];

impl OpenAiCompat {
    /// Ask the server what an image of a known size costs in prompt tokens.
    ///
    /// Guessing is not possible: a Qwen-VL charges by area, a Gemma charges a
    /// flat rate per image whatever its size. Two throwaway requests — one
    /// with a picture, one without — and the difference is the answer, for
    /// this model, from the server that will actually be billed for it.
    ///
    /// Returns tokens per pixel and the flat cost, as `(per_pixel, fixed)`.
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
        // The 1×1 costs whatever a picture costs before its pixels are
        // counted; the rest scales with area. A model that charges a flat
        // rate lands on per_pixel ≈ 0 by itself.
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
                Ok(resp) => return Ok(resp),
                // Connection failures only: a timeout after connecting means
                // the server may already be generating for this request.
                Err(e) if attempt < retries && e.is_connect() => {
                    attempt += 1;
                    tokio::time::sleep(Duration::from_millis(400 * attempt as u64)).await;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

/// Error message carried by an in-stream `{"error": ...}` payload, if any.
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
        // Every server accepts a plain string; only a message with images
        // needs the list form.
        let llm = OpenAiCompat::new("http://localhost:1234/v1", "m", None);
        // Thinking left to the server's default, so no per-turn note is added
        // to the text; the note has its own test.
        let options = crate::types::TurnOptions { thinking: crate::types::ThinkingEffort::Default, ..Default::default() };
        let body = llm.body(&[ChatMessage::user("hello")], &[], &options);
        let user = body["messages"].as_array().unwrap().iter().find(|m| m["role"] == "user").unwrap().clone();
        assert_eq!(user["content"], "hello");
    }

    #[test]
    fn switching_models_does_not_keep_the_old_model_s_thinking_profile() {
        // LM Studio warned: "'minimal' reasoning effort is not directly
        // supported" — we were still asking with the previous model's
        // profile after the server switched models under us.
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
        // The correction is a nudge, not a second opinion: a runaway value
        // would let one bad week silently pin the model at "off".
        let llm = OpenAiCompat::new("http://localhost:1234/v1", "m", None);
        assert_eq!(llm.effort_bias(), 0, "nothing learned yet");
        llm.set_effort_bias(-7);
        assert_eq!(llm.effort_bias(), -1);
        llm.set_effort_bias(7);
        assert_eq!(llm.effort_bias(), 1);
        llm.set_effort_bias(0);
        assert_eq!(llm.effort_bias(), 0);
    }

    /// One-shot HTTP server answering every request with `status` + `body`;
    /// returns its base URL and a request counter.
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
        // Watching the model list must wait for the model to finish, and
        // the model is not finished when the headers arrive.
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
        // The server's own 400 is an answer about reasoning; a model listing
        // that does not mention reasoning is not, and the look repeats every
        // 15 seconds. Forgetting the lesson would bring the 400 back.
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

        // An empty slot is filled, so the request does not fall back to a guess.
        let fresh = OpenAiCompat::new(b.base_url().to_string(), "m", None);
        fresh.discover_server().await.expect("the listing was read");
        assert!(fresh.profile().is_some_and(|p| p.is_unreported()));
    }

    #[tokio::test]
    async fn persistent_400_is_retried_a_bounded_number_of_times() {
        // "[3]" looks like a preset list to the error parser — the exact shape
        // that used to recurse forever.
        let (url, hits) = canned_server("400 Bad Request", r#"{"error":"invalid messages[3].content"}"#).await;
        let b = OpenAiCompat::new(url, "m", None);
        let res = b.stream(&[ChatMessage::user("hi")], &[]).await;
        assert!(matches!(res, Err(LlmError::Status { status: 400, .. })));
        assert!(hits.load(std::sync::atomic::Ordering::SeqCst) <= 1 + MAX_ADAPTIVE_RETRIES as usize);
    }

    #[tokio::test]
    async fn unrelated_400_never_becomes_a_thinking_profile_and_fallback_drops_extras() {
        // Rejects any body carrying `top_k` (like strict OpenAI-style APIs),
        // with an error text that happens to contain a bracketed number.
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

        // Auto mode (TurnOptions::default()): for "Hello!", dynamically selects "off"
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

        // Auto mode for coding task: dynamically selects "on"
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

        // Message contents are preserved verbatim across turns so earlier KV caches are never invalidated.
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

