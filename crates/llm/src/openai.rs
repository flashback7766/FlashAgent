//! The OpenAI chat-completions protocol, and every server that copies it: LM
//! Studio, llama.cpp, vLLM, Ollama's `/v1`, OpenRouter, OpenAI, DeepSeek, …
//! The client adapts to the server instead of assuming one: a field the server
//! refuses by name is left out from then on.

use std::time::Duration;

use crate::client::{pump, Client, EventStream, WireDecoder};
use crate::parse::{ChunkParser, SseDecoder};
use crate::thinking::{ServerDiscovery, ServerKind, ThinkingProfile, ThinkingProtocol};
use crate::types::{ChatMessage, LlmError, LlmEvent, Role, ToolSpec, TurnOptions};

/// Request fields that are not in every OpenAI-compatible API. A server that
/// names one in a 400/422 gets requests without it from then on. The thinking
/// switches are here too: a server that refuses one by name (Groq's
/// "`reasoning_effort` is not supported with this model") keeps its sampling
/// settings instead of losing them all to the fallback.
const OPTIONAL_FIELDS: [&str; 16] = [
    "temperature", "top_p", "top_k", "repeat_penalty", "repetition_penalty", "presence_penalty", "min_p",
    "stream_options", "cache_prompt", "prompt_cache",
    "reasoning_effort", "reasoning", "enable_thinking", "chat_template_kwargs", "chat_template_config", "thinking",
];

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Learned {
    fields: Option<Fields>,
    dropped: Vec<&'static str>,
    /// Newer OpenAI models take `max_completion_tokens` and refuse `max_tokens`.
    completion_tokens: bool,
    /// DeepSeek's reasoner refuses a history that carries its earlier reasoning.
    no_reasoning_replay: bool,
}

impl Learned {
    /// From the server's error text about the request it refused; false when
    /// it named nothing that request carried and we can drop.
    fn learn(&mut self, error: &str, sent: &serde_json::Value) -> bool {
        let error = error.to_lowercase();
        if !self.completion_tokens && error.contains("max_completion_tokens") {
            self.completion_tokens = true;
            return true;
        }
        let replayed = || sent["messages"].as_array().is_some_and(|m| m.iter().any(|m| m.get("reasoning_content").is_some()));
        if !self.no_reasoning_replay && mentions(&error, "reasoning_content") && replayed() {
            self.no_reasoning_replay = true;
            return true;
        }
        let named: Vec<&'static str> = OPTIONAL_FIELDS
            .iter()
            .copied()
            .filter(|f| !self.dropped.contains(f) && sent.get(*f).is_some() && mentions(&error, f))
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
        if self.no_reasoning_replay {
            for message in map.get_mut("messages").and_then(|m| m.as_array_mut()).into_iter().flatten() {
                if let Some(message) = message.as_object_mut() {
                    message.remove("reasoning_content");
                }
            }
        }
    }

    /// Merged, not replaced: a request running alongside (a subagent) may
    /// have learned something else meanwhile.
    fn merge_into(self, into: &mut Learned) {
        for field in self.dropped {
            if !into.dropped.contains(&field) {
                into.dropped.push(field);
            }
        }
        into.completion_tokens |= self.completion_tokens;
        into.no_reasoning_replay |= self.no_reasoning_replay;
        if self.fields.is_some() {
            into.fields = self.fields;
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

/// Where the API lives. A bare `http://host:port` means its `/v1`: that is
/// where every local server that copies OpenAI serves it (LM Studio, vLLM,
/// llama.cpp, Ollama, LocalAI, …), and several serve nothing at the root.
/// A URL with a path is taken as written.
pub(crate) fn api_base(client: &Client) -> String {
    let base = client.base_url();
    let has_path = base.split_once("://").map_or(base.as_str(), |(_, rest)| rest).contains('/');
    if has_path { base } else { format!("{base}/v1") }
}

pub(crate) fn headers(client: &Client) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(key) = client.api_key() {
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&format!("Bearer {key}")) {
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
    }
    headers
}

pub(crate) async fn stream(
    client: &Client,
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    options: &TurnOptions,
) -> Result<EventStream, LlmError> {
    // A 400 often means an optional field was rejected. Up to
    // MAX_ADAPTIVE_RETRIES times: learn the preset list if the error names one,
    // else drop fields (All -> NoExtraSampling -> Standard).
    let busy = client.busy();
    let url = format!("{}/chat/completions", api_base(client));
    let headers = headers(client);
    let mut learned = client.learned.read().clone();
    let known = learned.clone();
    let mut fields = learned.fields.unwrap_or(Fields::All);
    let mut adaptive_retries = 0u8;
    let resp = loop {
        let body = body_at(client, messages, tools, options, fields, &learned);
        let resp = client.post(&url, &headers, &body).await?;
        if resp.status().is_success() {
            // Kept only once a request without those fields went through: a
            // context overflow names no field and must not strip every request.
            if learned != known {
                learned.merge_into(&mut client.learned.write());
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
        match ThinkingProfile::parse_api_error(&body_text) {
            Some(taught) if fields == Fields::All && Some(&taught) != client.profile().as_ref() => client.set_profile(taught),
            _ => {
                if !learned.learn(&body_text, &body) {
                    if fields == Fields::Standard {
                        return Err(LlmError::Status { status, body: body_text });
                    }
                    fields = fields.fewer();
                    learned.fields = Some(fields);
                }
            }
        }
    };
    Ok(pump(resp, busy, Decoder::default()))
}

#[derive(Default)]
struct Decoder {
    sse: SseDecoder,
    parser: ChunkParser,
    /// Unknown until the first byte that is not blank: `{` is a server that
    /// ignored `stream: true` and answered with one JSON reply.
    whole_reply: Option<bool>,
    reply: Vec<u8>,
    failed: bool,
}

impl Decoder {
    fn payloads(&mut self, payloads: Vec<String>) -> Vec<Result<LlmEvent, LlmError>> {
        let mut events = Vec::new();
        for payload in payloads {
            if self.failed {
                break;
            }
            if payload.trim() == "[DONE]" {
                self.parser.end(&mut events);
                continue;
            }
            // Parsed once, for the error check and the events both.
            let Ok(chunk) = serde_json::from_str::<serde_json::Value>(&payload) else { continue };
            // Failures after the 200 (context overflow, crash) arrive in-stream.
            if let Some(msg) = stream_error(&chunk) {
                self.failed = true;
                let mut out: Vec<_> = events.into_iter().map(Ok).collect();
                out.push(Err(LlmError::Stream(msg)));
                return out;
            }
            // Tool calls written as text are caught by the loop's scanner, not here.
            self.parser.feed_value(chunk, &mut events);
        }
        events.into_iter().map(Ok).collect()
    }
}

impl WireDecoder for Decoder {
    fn feed(&mut self, bytes: &[u8]) -> Vec<Result<LlmEvent, LlmError>> {
        if self.whole_reply.is_none() {
            let first = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes).iter().find(|b| !b.is_ascii_whitespace());
            self.whole_reply = first.map(|&b| b == b'{');
        }
        if self.whole_reply == Some(true) {
            self.reply.extend_from_slice(bytes);
            return Vec::new();
        }
        let payloads = self.sse.feed(bytes);
        self.payloads(payloads)
    }

    fn finish(&mut self) -> Vec<Result<LlmEvent, LlmError>> {
        let payloads = if self.whole_reply == Some(true) {
            let reply = String::from_utf8_lossy(&self.reply).into_owned();
            // One reply, or one chunk per line from a server that streams without SSE.
            if serde_json::from_str::<serde::de::IgnoredAny>(&reply).is_ok() {
                vec![reply]
            } else {
                reply.lines().filter(|l| !l.trim().is_empty()).map(str::to_string).collect()
            }
        } else {
            self.sse.finish()
        };
        let mut out = self.payloads(payloads);
        if !self.failed {
            out.extend(self.parser.finish().into_iter().map(Ok));
        }
        out
    }
}

fn body_at(
    client: &Client,
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    options: &TurnOptions,
    fields: Fields,
    learned: &Learned,
) -> serde_json::Value {
    let current_model = client.model();
    let current_profile = client.profile().unwrap_or_default();
    let resolved_effort = client.resolve_effort(messages, options);

    // Kind of server, not its address: a remote llama-server still caches
    // prompts, a hosted API tunnelled to localhost does not.
    let is_local_or_lmstudio = client.server_kind().runs_local_models()
        || current_profile.protocol == ThinkingProtocol::LmStudio
        || current_profile.protocol == ThinkingProtocol::BooleanFlag;

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
        return finish(body, tools, learned);
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

    // Gemini shows its thinking only when asked to, effort or not.
    if resolved_effort.is_none() && current_profile.protocol == ThinkingProtocol::Gemini {
        current_profile.apply_to_request(&mut body, "auto");
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

    finish(body, tools, learned)
}

fn finish(mut body: serde_json::Value, tools: &[ToolSpec], learned: &Learned) -> serde_json::Value {
    learned.apply(&mut body);
    with_tools(body, tools)
}

fn with_tools(mut body: serde_json::Value, tools: &[ToolSpec]) -> serde_json::Value {
    if !tools.is_empty() {
        body["tools"] = tools_json(tools);
    }
    body
}

/// The function-tool list, which Ollama's own API takes as it is.
pub(crate) fn tools_json(tools: &[ToolSpec]) -> serde_json::Value {
    serde_json::Value::Array(
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
    )
}

/// Google's own list beside its compatible endpoint
/// (`…/v1beta/openai` → `…/v1beta/models`), asked with Google's key header:
/// it is the one that knows each model's context and whether it thinks.
async fn discover_gemini(client: &Client) -> Option<ServerDiscovery> {
    let base = client.base_url();
    let root = base.strip_suffix("/openai")?;
    if !root.contains("generativelanguage.googleapis.com") {
        return None;
    }
    let url = format!("{root}/models?pageSize=1000");
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(value) = client.api_key().and_then(|k| reqwest::header::HeaderValue::from_str(&k).ok()) {
        headers.insert("x-goog-api-key", value);
    }
    let val = client.get_json(&url, &headers, Duration::from_secs(5)).await?;
    apply_listing(client, &url, &val, None)
}

/// Tries LM Studio's `/api/v1/models` and `/api/v0/models`, then the standard `/models`.
pub(crate) async fn discover(client: &Client) -> Option<ServerDiscovery> {
    if let Some(disc) = discover_gemini(client).await {
        return Some(disc);
    }
    let base = api_base(client);
    let root = base.strip_suffix("/v1").unwrap_or(&base).to_string();
    let headers = headers(client);
    let probe = |url: String| {
        let headers = headers.clone();
        async move {
            let val = client.get_json(&url, &headers, Duration::from_millis(1500)).await;
            (url, val)
        }
    };
    let v0_url = format!("{root}/api/v0/models");

    let cached = client.working_models_url.read().clone();
    if let Some(url) = cached {
        if let (_, Some(val)) = probe(url.clone()).await {
            let extra = if url.ends_with("/api/v1/models") { probe(v0_url.clone()).await.1 } else { None };
            if let Some(disc) = apply_listing(client, &url, &val, extra.as_ref()) {
                return Some(disc);
            }
        }
        *client.working_models_url.write() = None;
    }

    let mut candidates: Vec<String> = Vec::with_capacity(4);
    for url in [format!("{root}/api/v1/models"), v0_url.clone(), format!("{base}/models"), format!("{root}/models")] {
        if !candidates.contains(&url) {
            candidates.push(url);
        }
    }
    let results = futures::future::join_all(candidates.into_iter().map(probe)).await;
    let v0 = results.iter().find(|(u, v)| *u == v0_url && v.is_some()).and_then(|(_, v)| v.as_ref());
    for (url, val) in &results {
        let Some(val) = val else { continue };
        let extra = if url.ends_with("/api/v1/models") { v0 } else { None };
        if let Some(disc) = apply_listing(client, url, val, extra) {
            return Some(disc);
        }
    }
    None
}

/// A model list that answered at `url`; `extra_v0` is LM Studio's older list,
/// which knows things the newer one does not.
fn apply_listing(
    client: &Client,
    url: &str,
    val: &serde_json::Value,
    extra_v0: Option<&serde_json::Value>,
) -> Option<ServerDiscovery> {
    // OpenRouter's list is also at `/api/v1/models`; only LM Studio's answer
    // has a `models` array or a per-model load `state`.
    let lm_studio_shape = val.get("models").is_some_and(|m| m.is_array())
        || val["data"].as_array().is_some_and(|d| d.iter().any(|m| m.get("state").is_some() || m.get("loaded_instances").is_some()));
    let kind = if (url.ends_with("/api/v1/models") || url.ends_with("/api/v0/models")) && lm_studio_shape {
        ServerKind::LmStudio
    } else if val["data"].as_array().is_some_and(|d| d.iter().any(|m| m["owned_by"] == "llamacpp")) {
        ServerKind::LlamaCpp
    } else {
        ServerKind::Other
    };

    let mut models = crate::thinking::parse_server_models(val);
    if let Some(extra) = extra_v0 {
        models = crate::thinking::merge_server_models(models, crate::thinking::parse_server_models(extra));
    }
    if models.is_empty() {
        return None;
    }
    *client.working_models_url.write() = Some(url.to_string());
    client.settle_discovery(models, kind)
}

/// The 1×1 is the control for the 64×64.
const PROBE_TINY: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R', 0, 0, 0, 1,
    0, 0, 0, 1, 8, 2, 0, 0, 0, 0x90, 0x77, 0x53, 0xDE, 0, 0, 0, 12, b'I', b'D', b'A', b'T', 0x08,
    0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB0, 0, 0,
    0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82,
];

/// Two requests, with and without a picture, give the difference; the 1×1
/// is the control.
pub(crate) async fn measure_image_cost(
client: &Client,
probe_png: &[u8], width: u32, height: u32) -> Option<(f32, f32)> {
    let _busy = client.busy();
    let url = format!("{}/chat/completions", api_base(client));
    let headers = headers(client);
    let ask = |images: Vec<String>| {
        let mut msg = ChatMessage::user("x");
        msg.images = images;
        serde_json::json!({
            "model": client.model(),
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

    let plain: serde_json::Value = client.post(&url, &headers, &ask(Vec::new())).await.ok()?.json().await.ok()?;
    let with_image: serde_json::Value =
        client.post(&url, &headers, &ask(vec![data_url(probe_png)])).await.ok()?.json().await.ok()?;
    let with_tiny: serde_json::Value =
        client.post(&url, &headers, &ask(vec![data_url(PROBE_TINY)])).await.ok()?.json().await.ok()?;

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

pub(crate) fn stream_error(v: &serde_json::Value) -> Option<String> {
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

/// A local server for tests that answers every request from a script and
/// keeps what it was sent.
#[cfg(test)]
pub(crate) mod test_server {
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Debug, Clone)]
    pub(crate) struct Request {
        pub method: String,
        pub path: String,
        pub body: String,
    }

    impl Request {
        pub fn json(&self) -> serde_json::Value {
            serde_json::from_str(&self.body).unwrap_or_default()
        }
    }

    pub(crate) type Log = Arc<Mutex<Vec<Request>>>;

    /// `answer` gives the status line, the content type and the body. The
    /// address has no path.
    pub(crate) async fn serve<F>(answer: F) -> (String, Log)
    where
        F: Fn(&Request) -> (&'static str, &'static str, String) + Send + Sync + 'static,
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let log: Log = Arc::default();
        let seen = log.clone();
        let answer = Arc::new(answer);
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let (seen, answer) = (seen.clone(), answer.clone());
                tokio::spawn(async move {
                    let Some(req) = read_request(&mut sock).await else { return };
                    seen.lock().unwrap().push(req.clone());
                    let (status, content_type, body) = answer(&req);
                    let head = format!("HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
                    let _ = sock.write_all(head.as_bytes()).await;
                    let _ = sock.write_all(body.as_bytes()).await;
                });
            }
        });
        (format!("http://{addr}"), log)
    }

    async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<Request> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 16384];
        let head_end = loop {
            let n = sock.read(&mut chunk).await.ok()?;
            if n == 0 {
                return None;
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break at + 4;
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        let length = head
            .lines()
            .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok()))
            .unwrap_or(0);
        while buf.len() < head_end + length {
            let n = sock.read(&mut chunk).await.ok()?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let mut first = head.lines().next()?.split_whitespace();
        Some(Request {
            method: first.next()?.to_string(),
            path: first.next()?.to_string(),
            body: String::from_utf8_lossy(&buf[head_end..]).to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ApiProtocol, Endpoint};
    use crate::types::FinishReason;
    use crate::LlmBackend;
    use futures::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn client(url: impl Into<String>, model: &str) -> Client {
        Client::new(Endpoint::new(ApiProtocol::OpenAi, url, None), model)
    }

    fn body(client: &Client, messages: &[ChatMessage], tools: &[ToolSpec], options: &TurnOptions) -> serde_json::Value {
        let learned = client.learned.read().clone();
        body_at(client, messages, tools, options, Fields::All, &learned)
    }

    #[test]
    fn a_message_with_a_picture_is_sent_as_content_parts() {
        let llm = client("http://localhost:1234/v1", "vlm");
        let mut msg = ChatMessage::user("why does this frame look wrong?");
        msg.images.push("data:image/png;base64,AAAA".to_string());
        let body = body(&llm, &[msg], &[], &TurnOptions::default());
        let user = body["messages"].as_array().unwrap().iter().find(|m| m["role"] == "user").unwrap().clone();
        let parts = user["content"].as_array().expect("content is a list of parts");
        assert_eq!(parts[0]["type"], "text", "the question comes before the picture it is about");
        assert_eq!(parts[0]["text"], "why does this frame look wrong?");
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,AAAA");
    }

    #[test]
    fn an_image_only_message_has_no_text_part() {
        let llm = client("http://localhost:1234/v1", "vlm");
        let mut msg = ChatMessage::user("");
        msg.images.push("data:image/png;base64,AAAA".to_string());
        let body = body(&llm, &[msg], &[], &TurnOptions::default());
        let user = body["messages"].as_array().unwrap().iter().find(|m| m["role"] == "user").unwrap().clone();
        let parts = user["content"].as_array().expect("content is a list of parts");
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["type"], "image_url");
        assert_eq!(parts[0]["image_url"]["url"], "data:image/png;base64,AAAA");
    }


    #[test]
    fn a_message_without_pictures_is_sent_exactly_as_before() {
        let llm = client("http://localhost:1234/v1", "m");
        let options = TurnOptions { thinking: crate::types::ThinkingEffort::Default, ..Default::default() };
        let body = body(&llm, &[ChatMessage::user("hello")], &[], &options);
        let user = body["messages"].as_array().unwrap().iter().find(|m| m["role"] == "user").unwrap().clone();
        assert_eq!(user["content"], "hello");
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
        let b = client(format!("http://{addr}/v1"), "m");
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
        let b = client(url, "m");
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
        let b = client(url, "m").with_profile(learned.clone());
        let disc = b.discover_server().await.expect("the listing was read");
        assert!(disc.models[0].thinking.is_unreported(), "the listing says nothing about reasoning");
        assert_eq!(b.profile(), Some(learned), "a look that said nothing replaced what the server's error taught");

        let fresh = client(b.base_url(), "m");
        fresh.discover_server().await.expect("the listing was read");
        assert!(fresh.profile().is_some_and(|p| p.is_unreported()));
    }

    #[tokio::test]
    async fn persistent_400_is_retried_a_bounded_number_of_times() {
        // "[3]" looks like a preset list to the parser; this used to recurse forever.
        let (url, hits) = canned_server("400 Bad Request", r#"{"error":"invalid messages[3].content"}"#).await;
        let b = client(url, "m");
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
        let b = client(format!("http://{addr}/v1"), "m");
        let opts = TurnOptions { top_k: Some(20), temperature: Some(0.5), ..Default::default() };
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
        let b = client(format!("http://{addr}/v1"), "m");
        let opts = TurnOptions { top_k: Some(20), top_p: Some(0.9), max_tokens: Some(100), ..Default::default() };
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
        let b = client(url, "m");
        let opts = TurnOptions { top_k: Some(20), ..Default::default() };
        assert!(b.stream_with_options(&[ChatMessage::user("hi")], &[], &opts).await.is_err());
        let learned = b.learned.read().clone();
        assert!(learned.fields.is_none() && learned.dropped.is_empty(), "{learned:?}");
        assert!(body(&b, &[ChatMessage::user("hi")], &[], &opts).get("top_k").is_some());
    }

    #[test]
    fn discovery_keeps_the_model_named_exactly_and_knows_openrouter_from_lm_studio() {
        let b = client("https://openrouter.ai/api/v1", "openai/gpt-4o");
        let listing = serde_json::json!({ "data": [
            { "id": "openai/gpt-4o-audio-preview", "context_length": 128000 },
            { "id": "openai/gpt-4o", "context_length": 128000 }
        ] });
        let disc = apply_listing(&b, "https://openrouter.ai/api/v1/models", &listing, None).expect("discovered");
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
        let b = client(url, "m");
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
        let b = client("http://localhost:1234/v1", "test-model");
        let body = body(&b, 
            &[ChatMessage::user("hi")],
            &[ToolSpec {
                name: "shell".into(),
                description: "run".into(),
                parameters_json: r#"{"type":"object"}"#.into(),
            }],
            &TurnOptions::default(),
        );
        assert_eq!(body["stream"], true);
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["tools"][0]["function"]["name"], "shell");
    }

    #[test]
    fn body_tool_result_shape() {
        let b = client("http://x/v1", "m");
        let body = body(&b, 
            &[
                ChatMessage::assistant("calling"),
                ChatMessage::tool_result("c1", "out"),
            ],
            &[],
            &TurnOptions::default(),
        );
        assert_eq!(body["messages"][1]["role"], "tool");
        assert_eq!(body["messages"][1]["tool_call_id"], "c1");
    }

    #[test]
    fn body_includes_thinking_effort_when_specified() {
        let b = client("http://localhost:1234/v1", "test-model");
        let body_low = body(&b, 
            &[ChatMessage::user("hi")],
            &[],
            &TurnOptions { thinking: crate::types::ThinkingEffort::Low, ..Default::default() },
        );
        assert_eq!(body_low["reasoning_effort"], "low");

        let b_binary = client("http://localhost:1234/v1", "binary-model")
            .with_profile(crate::thinking::ThinkingProfile {
                presets: vec!["off".into(), "on".into()],
                protocol: crate::thinking::ThinkingProtocol::BooleanFlag,
                supported: true,
                default_preset: None,
            });
        let body_binary = body(&b_binary, 
            &[ChatMessage::user("hi")],
            &[],
            &TurnOptions { thinking: crate::types::ThinkingEffort::Off, ..Default::default() },
        );
        assert_eq!(body_binary["enable_thinking"], false);

        let b_lm = client("http://localhost:1234/v1", "gemma-4")
            .with_profile(crate::thinking::ThinkingProfile {
                presets: vec!["off".into(), "on".into()],
                protocol: crate::thinking::ThinkingProtocol::LmStudio,
                supported: true,
                default_preset: Some("on".into()),
            });
        let body_lm_default = body(&b_lm, 
            &[ChatMessage::user("hi")],
            &[],
            &TurnOptions { thinking: crate::types::ThinkingEffort::Default, ..Default::default() },
        );
        assert_eq!(body_lm_default["reasoning"], "on");
        assert_eq!(body_lm_default["enable_thinking"], true);

        let sys_template = "You are FlashAgent. REASONING INSTRUCTIONS:\n- break down into bold stages\n\nTASK EXECUTION:\n- write clean code";
        let body_lm_auto_hi = body(&b_lm, 
            &[
                ChatMessage::system(sys_template),
                ChatMessage::user("Hello!"),
            ],
            &[],
            &TurnOptions::default(),
        );
        assert_eq!(body_lm_auto_hi["reasoning"], "off");
        assert_eq!(body_lm_auto_hi["enable_thinking"], false);
        assert_eq!(body_lm_auto_hi["chat_template_kwargs"]["enable_thinking"], false);
        let sys_hi = body_lm_auto_hi["messages"][0]["content"].as_str().unwrap();
        assert_eq!(sys_hi, sys_template, "the system prompt is never rewritten: it is the cached prefix");
        let user_hi = body_lm_auto_hi["messages"][1]["content"].as_str().unwrap();
        assert_eq!(user_hi, "Hello!", "user messages are never rewritten: keeps prefix KV cache valid");

        let body_lm_auto_code = body(&b_lm, 
            &[
                ChatMessage::system(sys_template),
                ChatMessage::user("Write a parser function in Rust"),
            ],
            &[],
            &TurnOptions::default(),
        );
        assert_eq!(body_lm_auto_code["reasoning"], "on");
        assert_eq!(body_lm_auto_code["enable_thinking"], true);
        assert_eq!(body_lm_auto_code["chat_template_kwargs"]["enable_thinking"], true);
        let sys_code = body_lm_auto_code["messages"][0]["content"].as_str().unwrap();
        assert_eq!(sys_code, sys_hi, "thinking on or off, the prompt starts with the same bytes");
        assert_eq!(body_lm_auto_code["messages"][1]["content"], "Write a parser function in Rust");

        let body_later = body(&b_lm, 
            &[
                ChatMessage::system(sys_template),
                ChatMessage::user("Write a parser function in Rust"),
                ChatMessage::assistant("Done."),
                ChatMessage::user("thanks"),
            ],
            &[],
            &TurnOptions { thinking: crate::types::ThinkingEffort::Off, ..Default::default() },
        );
        assert_eq!(body_later["messages"][0]["content"], sys_template);
        assert_eq!(body_later["messages"][1]["content"], "Write a parser function in Rust");
        assert_eq!(body_later["messages"][3]["content"], "thanks");

        let body_lm_off = body(&b_lm, 
            &[ChatMessage::user("hi")],
            &[],
            &TurnOptions { thinking: crate::types::ThinkingEffort::Off, ..Default::default() },
        );
        assert_eq!(body_lm_off["reasoning"], "off");
        assert_eq!(body_lm_off["enable_thinking"], false);

        let b_xhigh = client("http://localhost:1234/v1", "xhigh-model")
            .with_profile(crate::thinking::ThinkingProfile {
                presets: vec!["low".into(), "high".into(), "xhigh".into()],
                protocol: crate::thinking::ThinkingProtocol::ReasoningEffort,
                supported: true,
                default_preset: None,
            });
        let body_min = body(&b_xhigh, 
            &[ChatMessage::user("hi")],
            &[],
            &TurnOptions { thinking: crate::types::ThinkingEffort::Off, ..Default::default() },
        );
        assert_eq!(body_min["reasoning_effort"], "low");
        let body_max = body(&b_xhigh, 
            &[ChatMessage::user("hi")],
            &[],
            &TurnOptions { thinking: crate::types::ThinkingEffort::High, ..Default::default() },
        );
        assert_eq!(body_max["reasoning_effort"], "xhigh");

        let body_custom = body(&b_xhigh, 
            &[ChatMessage::user("hi")],
            &[],
            &TurnOptions {
                custom_effort: Some("xhigh".into()),
                ..Default::default()
            },
        );
        assert_eq!(body_custom["reasoning_effort"], "xhigh");
    }

    async fn collect(stream: EventStream) -> Vec<LlmEvent> {
        stream.map(|e| e.expect("no stream error")).collect().await
    }

    fn text_of(events: &[LlmEvent]) -> String {
        events.iter().filter_map(|e| match e { LlmEvent::TextDelta(t) => Some(t.as_str()), _ => None }).collect()
    }

    fn sse(body: &str) -> (&'static str, &'static str, String) {
        ("200 OK", "text/event-stream", body.to_string())
    }

    #[tokio::test]
    async fn an_address_typed_without_v1_still_reaches_the_api() {
        // vLLM and LM Studio serve nothing at the root: `http://localhost:8000` means its `/v1`.
        let (url, log) = test_server::serve(|req| match req.path.as_str() {
            "/v1/models" => ("200 OK", "application/json", r#"{"object":"list","data":[{"id":"Qwen/Qwen3-8B","owned_by":"vllm","max_model_len":40960}]}"#.into()),
            "/v1/chat/completions" => sse("data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"),
            _ => ("404 Not Found", "text/plain", "not found".into()),
        })
        .await;
        let b = client(&url, "");
        let disc = b.discover_server().await.expect("the list under /v1 was found");
        assert_eq!(b.model(), "Qwen/Qwen3-8B");
        assert_eq!(disc.models[0].context_length, Some(40960), "vLLM's max_model_len is the window it serves");
        let events = collect(b.stream(&[ChatMessage::user("hi")], &[]).await.unwrap()).await;
        assert_eq!(text_of(&events), "ok");
        assert!(log.lock().unwrap().iter().any(|r| r.method == "POST" && r.path == "/v1/chat/completions"));
        // A URL with a path is used as written.
        assert_eq!(api_base(&client("https://openrouter.ai/api/v1", "m")), "https://openrouter.ai/api/v1");
        assert_eq!(api_base(&client("http://localhost:8080", "m")), "http://localhost:8080/v1");
    }

    #[tokio::test]
    async fn a_server_that_ignores_streaming_is_read_from_its_one_reply() {
        let reply = r#"{"id":"x","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"Reading it.","reasoning_content":"plan","tool_calls":[{"id":"c1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a.rs\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":50,"completion_tokens":9}}"#;
        let (url, _) = test_server::serve(move |_| ("200 OK", "application/json", reply.to_string())).await;
        let events = collect(client(format!("{url}/v1"), "m").stream(&[ChatMessage::user("hi")], &[]).await.unwrap()).await;
        assert!(events.contains(&LlmEvent::ReasoningDelta("plan".into())));
        assert_eq!(text_of(&events), "Reading it.");
        assert!(events.iter().any(|e| matches!(e, LlmEvent::ToolCallDelta { name: Some(n), args_delta, .. } if n == "read_file" && args_delta == r#"{"path":"a.rs"}"#)));
        assert!(events.iter().any(|e| matches!(e, LlmEvent::Usage(u) if u.prompt == Some(50))));
        assert!(events.contains(&LlmEvent::Done(FinishReason::ToolUse)));
    }

    #[tokio::test]
    async fn the_last_event_of_a_body_without_a_closing_blank_line_is_read() {
        // Without it the reason was lost and the pump said Stop for an answer cut at the limit.
        let (url, _) = test_server::serve(|_| sse("data: {\"choices\":[{\"delta\":{\"content\":\"par\"}}]}\r\n\r\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}")).await;
        let events = collect(client(format!("{url}/v1"), "m").stream(&[ChatMessage::user("hi")], &[]).await.unwrap()).await;
        assert_eq!(events, vec![LlmEvent::TextDelta("par".into()), LlmEvent::Done(FinishReason::Length)]);
    }

    #[tokio::test]
    async fn an_error_after_some_text_keeps_the_text_and_ends_the_stream() {
        let (url, _) = test_server::serve(|_| sse("data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"b\"}}]}\n\ndata: {\"error\":{\"message\":\"upstream died\"}}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"c\"}}]}\n\n")).await;
        let items: Vec<_> = client(format!("{url}/v1"), "m").stream(&[ChatMessage::user("hi")], &[]).await.unwrap().collect().await;
        assert!(matches!(&items[..], [Ok(LlmEvent::TextDelta(a)), Ok(LlmEvent::TextDelta(b)), Err(LlmError::Stream(e))] if a == "a" && b == "b" && e == "upstream died"), "{items:?}");
    }

    #[tokio::test]
    async fn reasoning_the_server_refuses_to_be_sent_back_is_left_out_from_then_on() {
        let (url, log) = test_server::serve(|req| {
            if req.body.contains("\"reasoning_content\"") {
                ("400 Bad Request", "application/json", r#"{"error":{"message":"The reasoning_content field is not allowed in input messages","type":"invalid_request_error"}}"#.into())
            } else {
                sse("data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n")
            }
        })
        .await;
        let b = client(format!("{url}/v1"), "deepseek-reasoner");
        let mut earlier = ChatMessage::assistant("4");
        earlier.reasoning = Some("2+2 is 4".into());
        let history = [ChatMessage::user("2+2?"), earlier, ChatMessage::user("and 3+3?")];
        let opts = TurnOptions { top_p: Some(0.9), ..Default::default() };
        for _ in 0..2 {
            assert_eq!(text_of(&collect(b.stream_with_options(&history, &[], &opts).await.unwrap()).await), "ok");
        }
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 3, "one refusal, then never again");
        assert!(log[2].json()["top_p"].is_number(), "nothing else was given up: {}", log[2].body);
    }

    #[tokio::test]
    async fn a_thinking_switch_refused_by_name_is_dropped_without_the_sampling_settings() {
        let (url, log) = test_server::serve(|req| {
            if req.body.contains("\"reasoning_effort\"") {
                ("400 Bad Request", "application/json", r#"{"error":{"message":"`reasoning_effort` is not supported with this model","type":"invalid_request_error"}}"#.into())
            } else {
                sse("data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n")
            }
        })
        .await;
        let b = client(format!("{url}/v1"), "llama-3.3-70b-versatile").with_profile(ThinkingProfile {
            presets: vec!["low".into(), "medium".into(), "high".into()],
            protocol: ThinkingProtocol::ReasoningEffort,
            supported: true,
            default_preset: None,
        });
        let opts = TurnOptions { thinking: crate::types::ThinkingEffort::High, top_p: Some(0.9), top_k: Some(20), ..Default::default() };
        assert_eq!(text_of(&collect(b.stream_with_options(&[ChatMessage::user("hi")], &[], &opts).await.unwrap()).await), "ok");
        let log = log.lock().unwrap();
        let last = log.last().unwrap().json();
        assert!(last.get("reasoning_effort").is_none() && last["top_p"].is_number() && last["top_k"].is_number(), "{last}");
    }

    #[test]
    fn cloud_model_lists_say_what_each_model_takes() {
        let b = client("https://openrouter.ai/api/v1", "deepseek/deepseek-r1");
        let openrouter = serde_json::json!({ "data": [
            { "id": "deepseek/deepseek-r1", "context_length": 163840, "architecture": { "input_modalities": ["text"] },
              "supported_parameters": ["max_tokens", "reasoning", "include_reasoning", "tools", "tool_choice"] },
            { "id": "openai/gpt-4o", "context_length": 128000, "architecture": { "input_modalities": ["text", "image"] },
              "supported_parameters": ["max_tokens", "tools"] }
        ] });
        let disc = apply_listing(&b, "https://openrouter.ai/api/v1/models", &openrouter, None).unwrap();
        let r1 = &disc.models[0];
        assert!(r1.supports_tools && !r1.supports_vision && r1.context_length == Some(163840));
        assert_eq!(r1.thinking.protocol, ThinkingProtocol::ReasoningObject);
        assert!(r1.thinking.supported);
        let gpt = &disc.models[1];
        assert!(gpt.supports_vision && gpt.thinking.is_unreported(), "no reasoning parameter: nothing claimed");

        let groq = serde_json::json!({ "object": "list", "data": [ { "id": "llama-3.3-70b-versatile", "owned_by": "Meta", "context_window": 131072, "active": true } ] });
        let disc = apply_listing(&client("https://api.groq.com/openai/v1", "m"), "https://api.groq.com/openai/v1/models", &groq, None).unwrap();
        assert_eq!(disc.models[0].context_length, Some(131072));

        let mistral = serde_json::json!({ "object": "list", "data": [
            { "id": "mistral-embed", "capabilities": { "completion_chat": false, "function_calling": false }, "max_context_length": 8192 },
            { "id": "pixtral-large-latest", "capabilities": { "completion_chat": true, "function_calling": true, "vision": true }, "max_context_length": 131072 }
        ] });
        let disc = apply_listing(&client("https://api.mistral.ai/v1", "m"), "https://api.mistral.ai/v1/models", &mistral, None).unwrap();
        assert_eq!(disc.models.len(), 1, "the embedding model is not offered for chat");
        assert!(disc.models[0].supports_tools && disc.models[0].supports_vision);
        assert_eq!(disc.models[0].max_context_length, Some(131072));
    }

    #[tokio::test]
    async fn a_refusal_naming_a_field_the_request_did_not_carry_wastes_no_retry() {
        // The error says "thinking", but no `thinking` field was sent: dropping it would
        // change nothing and spend one of the few retries.
        let (url, log) = test_server::serve(|req| {
            if req.body.contains("\"enable_thinking\"") {
                ("400 Bad Request", "application/json", r#"{"error":"thinking is not supported by this model"}"#.into())
            } else {
                sse("data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n")
            }
        })
        .await;
        let b = client(format!("{url}/v1"), "m").with_profile(ThinkingProfile {
            presets: vec!["off".into(), "on".into()],
            protocol: ThinkingProtocol::LmStudio,
            supported: true,
            default_preset: Some("on".into()),
        });
        let opts = TurnOptions { thinking: crate::types::ThinkingEffort::Off, ..Default::default() };
        assert_eq!(text_of(&collect(b.stream_with_options(&[ChatMessage::user("hi")], &[], &opts).await.unwrap()).await), "ok");
        assert_eq!(log.lock().unwrap().len(), 3, "all fields, fewer, standard");
    }
}
