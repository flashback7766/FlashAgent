//! Google's Gemini API (`models/{model}:streamGenerateContent`), spoken
//! natively: thoughts arrive marked as thoughts, function calls arrive whole,
//! and the thought signatures Gemini 3 insists on are kept on the message
//! ([`ChatMessage::replay`]) and sent back on the parts they came with.
//! Google's OpenAI-compatible gateway is served by the `openai` module.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::client::{pump, Client, EventStream, WireDecoder};
use crate::parse::SseDecoder;
use crate::thinking::{
    gemini_profile, gemini_thinking_config, gemini_version, parse_gemini_models, DiscoveredModel, ServerDiscovery, ServerKind,
    ThinkingProfile, ThinkingProtocol,
};
use crate::types::{ChatMessage, FinishReason, LlmError, LlmEvent, Role, ToolSpec, TurnOptions, Usage};

/// Google's documented placeholder for a function call it did not sign
/// (history from another model or provider). Gemini 3 refuses a call of the
/// current turn that carries no signature at all.
const UNSIGNED: &str = "skip_thought_signature_validator";

/// A refused thinking setting is asked again with less, at most this often.
const THINKING_RETRIES: u8 = 2;

/// A page holds 1000 models; no real listing comes near this.
const MAX_PAGES: usize = 10;

/// What a model refused outright (Gemma: function declarations, a system
/// instruction), by address and model, so it is not sent to it again.
static REFUSED: std::sync::LazyLock<parking_lot::Mutex<std::collections::HashSet<(String, String, Refused)>>> =
    std::sync::LazyLock::new(Default::default);

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Refused {
    Tools,
    SystemInstruction,
}

fn refused(client: &Client, model: &str, what: Refused) -> bool {
    REFUSED.lock().contains(&(client.base_url(), model.to_string(), what))
}

fn headers(client: &Client) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    // A header, never `?key=`: addresses end up in logs and error messages.
    if let Some(value) = client.api_key().and_then(|k| reqwest::header::HeaderValue::from_str(&k).ok()) {
        headers.insert("x-goog-api-key", value);
    }
    headers
}

/// Google's address typed without a version means the current API.
fn api_root(base: &str) -> String {
    let base = base.trim_end_matches('/');
    let after_scheme = base.split_once("://").map_or(base, |(_, rest)| rest);
    if after_scheme.contains('/') { base.to_string() } else { format!("{base}/v1beta") }
}

/// The listing names models `models/…`; people write them without it. Both work.
fn model_path(model: &str) -> String {
    let model = model.trim();
    if model.starts_with("models/") || model.starts_with("tunedModels/") { model.to_string() } else { format!("models/{model}") }
}

fn short_name(model: &str) -> &str {
    model.strip_prefix("models/").unwrap_or(model)
}

/// Before the listing has said: 2.5 and later think, and so do aliases like
/// `gemini-flash-latest`, which point at the newest.
fn thinks_by_name(model: &str) -> bool {
    short_name(model).starts_with("gemini-") && gemini_version(model).is_none_or(|v| v >= (2, 5))
}

/// Gemini 3 and later validate the signatures of the current turn's calls;
/// 2.5 signs optionally and is sent nothing it did not give.
fn validates_signatures(model: &str) -> bool {
    model.contains("gemini-") && gemini_version(model).is_none_or(|(major, _)| major >= 3)
}

pub(crate) async fn stream(
    client: &Client,
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    options: &TurnOptions,
) -> Result<EventStream, LlmError> {
    let busy = client.busy();
    let url = format!("{}/{}:streamGenerateContent?alt=sse", api_root(&client.base_url()), model_path(&client.model()));
    let headers = headers(client);
    let mut body = request_body(client, messages, tools, options);
    let mut retries = 0u8;
    let resp = loop {
        let resp = client.post(&url, &headers, &body).await?;
        if resp.status().is_success() {
            break resp;
        }
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        let lower = text.to_lowercase();
        let refusal = [("function calling is not enabled", "tools", Refused::Tools), ("developer instruction is not enabled", "systemInstruction", Refused::SystemInstruction)]
            .into_iter()
            .find(|(said, field, _)| lower.contains(said) && body.get(field).is_some());
        if status == 400 && retries < THINKING_RETRIES + 2 {
            if let Some((_, _, what)) = refusal {
                REFUSED.lock().insert((client.base_url(), client.model(), what));
                body = request_body(client, messages, tools, options);
                retries += 1;
                continue;
            }
            if lower.contains("thinking") && think_less(client, &mut body) {
                retries += 1;
                continue;
            }
        }
        return Err(LlmError::Status { status, body: text });
    };
    Ok(pump(resp, busy, Decoder::default()))
}

/// For a model that refuses a system instruction: the instructions open the
/// first user message instead.
fn system_in_first_prompt(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    let system: Vec<String> = messages.iter().filter(|m| m.role == Role::System && !m.content.trim().is_empty()).map(|m| m.content.clone()).collect();
    let mut rest: Vec<ChatMessage> = messages.into_iter().filter(|m| m.role != Role::System).collect();
    if system.is_empty() {
        return rest;
    }
    let instructions = system.join("\n\n");
    match rest.iter_mut().find(|m| m.role == Role::User) {
        Some(first) => first.content = format!("{instructions}\n\n{}", first.content),
        None => rest.insert(0, ChatMessage::user(instructions)),
    }
    rest
}

/// The model refused its thinking settings: first the level or budget goes
/// (the model then thinks as it likes), then the whole config. What was
/// refused is not offered again for this model.
fn think_less(client: &Client, body: &mut Value) -> bool {
    let Some(config) = body.get_mut("generationConfig").and_then(Value::as_object_mut) else { return false };
    let Some(thinking) = config.get_mut("thinkingConfig").and_then(Value::as_object_mut) else { return false };
    let level = thinking.remove("thinkingLevel");
    let budget = thinking.remove("thinkingBudget");
    if level.is_some() || budget.is_some() {
        thinking.insert("includeThoughts".into(), json!(true));
        if let (Some(level), Some(mut profile)) = (level.as_ref().and_then(Value::as_str), client.profile()) {
            profile.presets.retain(|p| !p.eq_ignore_ascii_case(level));
            client.set_profile(if profile.presets.is_empty() { ThinkingProfile::unsupported() } else { profile });
        }
        return true;
    }
    config.remove("thinkingConfig");
    client.set_profile(ThinkingProfile::unsupported());
    true
}

/// Rebuilt the same way from the same history every time, so the opening
/// (system instruction, tools, earlier turns) stays byte-identical and
/// Gemini's implicit cache keeps hitting; only generation settings vary.
fn request_body(client: &Client, messages: &[ChatMessage], tools: &[ToolSpec], options: &TurnOptions) -> Value {
    let model = client.model();
    // Gemma, served here too, takes no function declarations: its tools go in
    // the prompt as text (see `text_tools`).
    let listed_without_tools = client.discovery().and_then(|d| d.model(&model).map(|m| !m.supports_tools)).unwrap_or(false);
    let in_text = !tools.is_empty() && (listed_without_tools || refused(client, &model, Refused::Tools));
    let mut messages = if in_text { crate::text_tools::flatten(messages, tools) } else { messages.to_vec() };
    if refused(client, &model, Refused::SystemInstruction) {
        messages = system_in_first_prompt(messages);
    }
    let mut body = json!({ "contents": contents(&messages, validates_signatures(&model)) });
    let system: Vec<Value> = messages
        .iter()
        .filter(|m| m.role == Role::System && !m.content.trim().is_empty())
        .map(|m| json!({ "text": m.content }))
        .collect();
    if !system.is_empty() {
        body["systemInstruction"] = json!({ "parts": system });
    }
    if !tools.is_empty() && !in_text {
        body["tools"] = json!([{ "functionDeclarations": tools.iter().map(declaration).collect::<Vec<_>>() }]);
    }
    // Repeat, presence and min-p penalties are local-model knobs: Gemini has
    // no min-p, and some of its models refuse a request that names a penalty.
    let mut config = serde_json::Map::new();
    if let Some(t) = options.temperature {
        config.insert("temperature".into(), json!(t));
    }
    if let Some(p) = options.top_p {
        config.insert("topP".into(), json!(p));
    }
    if let Some(k) = options.top_k {
        config.insert("topK".into(), json!(k));
    }
    if let Some(n) = options.max_tokens {
        config.insert("maxOutputTokens".into(), json!(n));
    }
    if let Some(thinking) = thinking_config(client, &messages, options) {
        config.insert("thinkingConfig".into(), thinking);
    }
    if !config.is_empty() {
        body["generationConfig"] = Value::Object(config);
    }
    body
}

/// The thinking asked for, with `includeThoughts`: without it Gemini thinks
/// but never shows it. `None` for a model that cannot think.
fn thinking_config(client: &Client, messages: &[ChatMessage], options: &TurnOptions) -> Option<Value> {
    let model = client.model();
    let profile = match client.profile() {
        Some(p) if p.protocol == ThinkingProtocol::Gemini || (!p.supported && !p.is_unreported()) => p,
        // Not listed yet: the family's own levels, so the effort resolves to one of them.
        _ => {
            let p = gemini_profile(&model, thinks_by_name(&model));
            client.set_profile(p.clone());
            p
        }
    };
    if !profile.supported {
        return None;
    }
    let config = match client.resolve_effort(messages, options) {
        Some(effort) => gemini_thinking_config(&model, &effort),
        None => json!({ "include_thoughts": true }),
    };
    // The gateway's `thinking_config`, in the native API's spelling.
    let Value::Object(fields) = config else { return None };
    let camel = fields.into_iter().map(|(key, value)| {
        let key = match key.as_str() {
            "thinking_budget" => "thinkingBudget".to_string(),
            "thinking_level" => "thinkingLevel".to_string(),
            "include_thoughts" => "includeThoughts".to_string(),
            _ => key,
        };
        (key, value)
    });
    Some(Value::Object(camel.collect()))
}

/// JSON Schema as the tool wrote it (`parametersJsonSchema`), not the
/// OpenAPI subset of `parameters`: MCP tools use `$ref`, `anyOf`, `const`
/// and type lists, which that subset would have to rewrite or lose.
fn declaration(tool: &ToolSpec) -> Value {
    let mut declaration = json!({ "name": tool.name, "description": tool.description });
    let Ok(Value::Object(mut schema)) = serde_json::from_str::<Value>(&tool.parameters_json) else { return declaration };
    // It names the meta-schema, not the parameters.
    schema.remove("$schema");
    let has_parameters = schema.get("properties").and_then(Value::as_object).is_some_and(|p| !p.is_empty())
        || ["$ref", "anyOf", "oneOf", "allOf"].iter().any(|k| schema.contains_key(*k));
    if has_parameters {
        schema.entry("type").or_insert_with(|| json!("object"));
        declaration["parametersJsonSchema"] = Value::Object(schema);
    }
    declaration
}

/// Consecutive messages of one role become one turn; a message with nothing
/// to send sends nothing (Gemini refuses empty parts).
fn contents(messages: &[ChatMessage], validates_signatures: bool) -> Vec<Value> {
    let mut contents: Vec<Value> = Vec::new();
    // The calls of the last model message, by id: a result is a function
    // response only right after the call it answers.
    let mut open: HashMap<&str, (&str, bool)> = HashMap::new();
    for m in messages {
        let (role, parts) = match m.role {
            Role::System => continue,
            Role::User => ("user", user_parts(m)),
            Role::Assistant => {
                let replay = Replay::read(m.replay.as_ref());
                open = m.tool_calls.iter().map(|c| (c.id.as_str(), (c.name.as_str(), replay.ids.contains(&c.id)))).collect();
                ("model", model_parts(m, &replay, validates_signatures))
            }
            Role::Tool => ("user", tool_parts(m, &open)),
        };
        if parts.is_empty() {
            continue;
        }
        match contents.last_mut() {
            Some(last) if last["role"] == role => {
                if let Some(earlier) = last["parts"].as_array_mut() {
                    earlier.extend(parts);
                }
            }
            _ => contents.push(json!({ "role": role, "parts": parts })),
        }
    }
    contents
}

/// Text first, so the question is read before the picture it is about.
fn user_parts(m: &ChatMessage) -> Vec<Value> {
    let mut parts = Vec::new();
    if !m.content.trim().is_empty() {
        parts.push(json!({ "text": m.content }));
    }
    parts.extend(m.images.iter().filter_map(|url| inline_data(url)));
    parts
}

/// `data:image/png;base64,…`; anything else is not something Gemini can read inline.
fn inline_data(url: &str) -> Option<Value> {
    let (meta, data) = url.strip_prefix("data:")?.split_once(',')?;
    let mime = meta.strip_suffix(";base64")?;
    Some(json!({ "inlineData": { "mimeType": if mime.is_empty() { "image/png" } else { mime }, "data": data } }))
}

/// Thoughts are not sent back: their signatures carry them.
fn model_parts(m: &ChatMessage, replay: &Replay, validates_signatures: bool) -> Vec<Value> {
    let mut parts = Vec::new();
    if !m.content.trim().is_empty() {
        let mut text = json!({ "text": m.content });
        if let Some(signature) = &replay.text {
            text["thoughtSignature"] = json!(signature);
        }
        parts.push(text);
    }
    for (i, call) in m.tool_calls.iter().enumerate() {
        let mut function = json!({ "name": call.name, "args": arguments(&call.args_json) });
        if replay.ids.contains(&call.id) {
            function["id"] = json!(call.id);
        }
        let mut part = json!({ "functionCall": function });
        // Gemini signs only the first of parallel calls.
        match replay.calls.get(&call.id) {
            Some(signature) => part["thoughtSignature"] = json!(signature),
            None if i == 0 && validates_signatures => part["thoughtSignature"] = json!(UNSIGNED),
            None => {}
        }
        parts.push(part);
    }
    parts
}

/// Always an object: Gemini takes nothing else, and a history holding a
/// broken call must still be sendable.
fn arguments(args_json: &str) -> Value {
    let parsed = crate::repair::repair_json(args_json).and_then(|s| serde_json::from_str::<Value>(&s).ok());
    match parsed {
        Some(Value::Object(args)) => Value::Object(args),
        // Encoded twice: `"{\"path\":…}"`.
        Some(Value::String(inner)) => serde_json::from_str::<Value>(&inner).ok().filter(Value::is_object).unwrap_or_else(|| json!({})),
        _ => json!({}),
    }
}

fn tool_parts(m: &ChatMessage, open: &HashMap<&str, (&str, bool)>) -> Vec<Value> {
    let id = m.tool_call_id.as_deref().unwrap_or_default();
    let first = match open.get(id) {
        Some((name, echo_id)) => {
            // The text as the tool wrote it, even when it is JSON: parsed, a
            // file shown with `cat` would reach the model re-ordered and
            // re-spaced, and an edit copied from it would not match the file.
            let mut response = json!({ "name": name, "response": { "result": m.content } });
            if *echo_id {
                response["id"] = json!(id);
            }
            json!({ "functionResponse": response })
        }
        // Its call is not in the model turn before it (the history was cut
        // there), and a function response without its call is refused.
        None => json!({ "text": format!("Result of an earlier tool call:\n{}", m.content) }),
    };
    let mut parts = vec![first];
    parts.extend(m.images.iter().filter_map(|url| inline_data(url)));
    parts
}

/// What Gemini wants back with an assistant message, kept on it as
/// [`ChatMessage::replay`].
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
struct Replay {
    /// Call id → the signature on that call's part.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    calls: BTreeMap<String, String>,
    /// The signature on a text or thought part, sent back on the text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    /// The call ids Gemini gave, echoed back; ids made up here are not.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    ids: Vec<String>,
}

impl Replay {
    /// Another protocol's state (the provider changed mid-session) reads as none.
    fn read(replay: Option<&Value>) -> Self {
        replay
            .filter(|r| r["protocol"] == "gemini")
            .and_then(|r| serde_json::from_value(r.clone()).ok())
            .unwrap_or_default()
    }

    fn to_value(&self) -> Value {
        let mut value = serde_json::to_value(self).unwrap_or_else(|_| json!({}));
        value["protocol"] = json!("gemini");
        value
    }
}

/// Gemini's API does not always name its calls, and results are matched to
/// calls by id.
fn new_call_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    format!("call_{nanos:x}_{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

#[derive(Default)]
struct Decoder {
    sse: SseDecoder,
    /// Function calls so far, which is also the index of the next.
    calls: usize,
    replay: Replay,
    /// Every chunk repeats the running totals; the last one is sent, once.
    usage: Option<Usage>,
    thought: crate::parse::ThoughtTags,
    done: bool,
}

impl WireDecoder for Decoder {
    fn feed(&mut self, bytes: &[u8]) -> Vec<Result<LlmEvent, LlmError>> {
        let mut out = Vec::new();
        for payload in self.sse.feed(bytes) {
            if !self.read(&payload, &mut out) {
                break;
            }
        }
        out
    }

    fn finish(&mut self) -> Vec<Result<LlmEvent, LlmError>> {
        // A last event the server did not close with a blank line.
        let mut out = self.feed(b"\n\n");
        self.flush_text(&mut out);
        self.flush_usage(&mut out);
        out
    }
}

impl Decoder {
    fn flush_text(&mut self, out: &mut Vec<Result<LlmEvent, LlmError>>) {
        let mut events = Vec::new();
        self.thought.flush(&mut events);
        out.extend(events.into_iter().map(Ok));
    }

    fn flush_usage(&mut self, out: &mut Vec<Result<LlmEvent, LlmError>>) {
        if let Some(usage) = self.usage.take() {
            out.push(Ok(LlmEvent::Usage(usage)));
        }
    }

    /// False once the stream has failed.
    fn read(&mut self, payload: &str, out: &mut Vec<Result<LlmEvent, LlmError>>) -> bool {
        if self.done {
            return true;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(payload) else { return true };
        // Failures after the 200 (quota, overload) arrive in-stream.
        if let Some(error) = chunk.get("error").filter(|e| !e.is_null()) {
            out.push(Err(LlmError::Stream(error_message(error))));
            return false;
        }
        if let Some(usage) = chunk.get("usageMetadata").and_then(read_usage) {
            self.usage = Some(usage);
        }
        let Some(candidate) = chunk.pointer("/candidates/0") else {
            let blocked = chunk.pointer("/promptFeedback/blockReason").and_then(Value::as_str).filter(|r| *r != "BLOCK_REASON_UNSPECIFIED");
            if let Some(reason) = blocked {
                self.flush_usage(out);
                out.push(Err(LlmError::Forbidden(format!("Gemini refused the request ({reason}) because {}", why_blocked(reason)))));
                return false;
            }
            return true;
        };
        let before = self.replay.clone();
        for part in candidate.pointer("/content/parts").and_then(Value::as_array).into_iter().flatten() {
            self.read_part(part, out);
        }
        if self.replay != before {
            out.push(Ok(LlmEvent::Replay(self.replay.to_value())));
        }
        let reason = candidate.get("finishReason").and_then(Value::as_str).unwrap_or_default();
        if !matches!(reason, "" | "FINISH_REASON_UNSPECIFIED") {
            self.flush_text(out);
        }
        let finish = match reason {
            "" | "FINISH_REASON_UNSPECIFIED" => return true,
            "STOP" if self.calls > 0 => FinishReason::ToolUse,
            "STOP" => FinishReason::Stop,
            "MAX_TOKENS" => FinishReason::Length,
            other => {
                self.flush_usage(out);
                out.push(Err(stopped(other, candidate.get("finishMessage").and_then(Value::as_str))));
                return false;
            }
        };
        self.flush_usage(out);
        out.push(Ok(LlmEvent::Done(finish)));
        self.done = true;
        true
    }

    fn read_part(&mut self, part: &Value, out: &mut Vec<Result<LlmEvent, LlmError>>) {
        let signature = part.get("thoughtSignature").and_then(Value::as_str).filter(|s| !s.is_empty());
        if let Some(call) = part.get("functionCall") {
            self.flush_text(out);
            let id = match call.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()) {
                Some(id) => {
                    self.replay.ids.push(id.to_string());
                    id.to_string()
                }
                None => new_call_id(),
            };
            if let Some(signature) = signature {
                self.replay.calls.insert(id.clone(), signature.to_string());
            }
            // Whole, never in pieces: the arguments are complete JSON.
            let args = call.get("args").filter(|a| a.is_object()).map_or_else(|| "{}".to_string(), Value::to_string);
            let name = call.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
            out.push(Ok(LlmEvent::ToolCallDelta { index: self.calls, id: Some(id), name: Some(name), args_delta: args }));
            self.calls += 1;
            return;
        }
        // Streaming, the signature of a plain answer often comes on a last, empty part.
        if let Some(signature) = signature {
            self.replay.text = Some(signature.to_string());
        }
        let text = part.get("text").and_then(Value::as_str).unwrap_or_default();
        if text.is_empty() {
            return;
        }
        let marked = part.get("thought").and_then(Value::as_bool).unwrap_or(false);
        let mut events = Vec::new();
        self.thought.split(text.to_string(), marked, &mut events);
        out.extend(events.into_iter().map(Ok));
    }
}

fn read_usage(meta: &Value) -> Option<Usage> {
    let count = |key: &str| meta.get(key).and_then(Value::as_i64);
    let prompt = count("promptTokenCount").map(|p| p + count("toolUsePromptTokenCount").unwrap_or(0));
    // Thoughts are generated and paid for like the answer.
    let completion = match (count("candidatesTokenCount"), count("thoughtsTokenCount")) {
        (None, None) => None,
        (answer, thoughts) => Some(answer.unwrap_or(0) + thoughts.unwrap_or(0)),
    };
    (prompt.is_some() || completion.is_some()).then_some(Usage { prompt, completion, cached: count("cachedContentTokenCount"), mtp: None })
}

fn error_message(error: &Value) -> String {
    let Some(message) = error.get("message").and_then(Value::as_str) else {
        return error.as_str().map_or_else(|| error.to_string(), str::to_string);
    };
    match error.get("status").and_then(Value::as_str) {
        Some(status) => format!("{message} ({status})"),
        None => message.to_string(),
    }
}

/// An answer Gemini withheld is `Forbidden`, with the reason; one that broke
/// down is a failed stream, which is worth asking again.
fn stopped(reason: &str, message: Option<&str>) -> LlmError {
    let (withheld, why) = match reason {
        "SAFETY" | "IMAGE_SAFETY" => (true, "its safety filters flagged it"),
        "RECITATION" | "IMAGE_RECITATION" => (true, "it repeated copyrighted text too closely"),
        "LANGUAGE" => (true, "it was in a language Gemini does not support"),
        "BLOCKLIST" => (true, "it contained a blocked term"),
        "PROHIBITED_CONTENT" | "IMAGE_PROHIBITED_CONTENT" => (true, "it may contain prohibited content"),
        "SPII" => (true, "it may contain sensitive personal information"),
        "ESCALATION" | "PUP_LIMITED_DISABLED" => (true, "the account or the request is restricted"),
        "MALFORMED_FUNCTION_CALL" => (false, "the model wrote a tool call that could not be read"),
        "UNEXPECTED_TOOL_CALL" => (false, "the model called a tool it was not offered"),
        "TOO_MANY_TOOL_CALLS" => (false, "the model made too many tool calls in a row"),
        "MISSING_THOUGHT_SIGNATURE" => (false, "a thought signature was missing from the history"),
        "MALFORMED_RESPONSE" => (false, "the answer came back malformed"),
        _ => (false, "of something it did not name"),
    };
    let detail = message.map(str::trim).filter(|m| !m.is_empty()).map(|m| format!(": {m}")).unwrap_or_default();
    let text = format!("Gemini stopped the answer ({reason}) because {why}{detail}");
    if withheld { LlmError::Forbidden(text) } else { LlmError::Stream(text) }
}

fn why_blocked(reason: &str) -> &'static str {
    match reason {
        "SAFETY" => "its safety filters flagged it",
        "BLOCKLIST" => "it contains a blocked term",
        "PROHIBITED_CONTENT" => "it may contain prohibited content",
        "IMAGE_SAFETY" => "an image in it was flagged",
        _ => "of something it did not name",
    }
}

/// `GET /models`, every page. Needs the key: Google lists nothing without one.
pub(crate) async fn discover(client: &Client, generation: u64) -> Option<ServerDiscovery> {
    client.api_key()?;
    // A model saved under the listing's long name is the same model.
    let current = client.model();
    if current.starts_with("models/") {
        client.set_model(short_name(&current));
    }
    let headers = headers(client);
    let list_url = format!("{}/models", api_root(&client.base_url()));
    let mut listed = Vec::new();
    let mut page_token: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut url = reqwest::Url::parse(&list_url).ok()?;
        url.query_pairs_mut().append_pair("pageSize", "1000");
        if let Some(token) = &page_token {
            url.query_pairs_mut().append_pair("pageToken", token);
        }
        // Every page or nothing: a model missing from half a list would be
        // replaced by another.
        let page = client.get_json(url.as_str(), &headers, Duration::from_secs(5)).await?;
        listed.extend(page.get("models").and_then(Value::as_array).cloned().unwrap_or_default());
        page_token = page.get("nextPageToken").and_then(Value::as_str).filter(|t| !t.is_empty()).map(str::to_string);
        if page_token.is_none() {
            break;
        }
    }
    let models = chat_models(&json!({ "models": listed }));
    client.settle_discovery(generation, models, ServerKind::Gemini, None)
}

/// Named the short way, as people write them. Models that only speak or
/// draw answer `generateContent` too, and are left out.
fn chat_models(listing: &Value) -> Vec<DiscoveredModel> {
    parse_gemini_models(listing)
        .into_iter()
        .filter(|m| !m.id.contains("-tts") && !m.id.contains("-image"))
        .map(|mut m| {
            m.id = short_name(&m.id).to_string();
            // Gemma, also served here, takes neither tools nor pictures.
            let gemini = m.id.starts_with("gemini-");
            m.supports_tools = gemini;
            m.supports_vision = gemini;
            m
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ApiProtocol, Endpoint};
    use crate::types::{ThinkingEffort, ToolCall};
    use crate::LlmBackend;
    use futures::StreamExt;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn client(model: &str) -> Client {
        Client::new(Endpoint::new(ApiProtocol::Gemini, "https://generativelanguage.googleapis.com/v1beta", Some("k".into())), model)
    }

    fn body(model: &str, messages: &[ChatMessage], tools: &[ToolSpec], options: &TurnOptions) -> Value {
        request_body(&client(model), messages, tools, options)
    }

    fn call(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall { id: id.into(), name: name.into(), args_json: args.into() }
    }

    fn calling(content: &str, calls: Vec<ToolCall>, replay: Option<Value>) -> ChatMessage {
        let mut m = ChatMessage::assistant(content);
        m.tool_calls = calls;
        m.replay = replay;
        m
    }

    fn sse(chunks: &[Value]) -> String {
        chunks.iter().map(|c| format!("data: {c}\r\n\r\n")).collect()
    }

    fn decode(pieces: &[&str]) -> Vec<Result<LlmEvent, LlmError>> {
        let mut decoder = Decoder::default();
        let mut out = Vec::new();
        for piece in pieces {
            out.extend(decoder.feed(piece.as_bytes()));
        }
        out.extend(decoder.finish());
        out
    }

    fn ok_events(items: Vec<Result<LlmEvent, LlmError>>) -> Vec<LlmEvent> {
        items.into_iter().map(|i| i.expect("no error")).collect()
    }

    #[test]
    fn system_messages_become_the_system_instruction_and_turns_alternate_between_user_and_model() {
        let body = body(
            "gemini-3-flash",
            &[
                ChatMessage::system("You are FlashAgent."),
                ChatMessage::user("first"),
                ChatMessage::user("second"),
                ChatMessage::assistant("an answer"),
                ChatMessage::user("thanks"),
            ],
            &[],
            &TurnOptions::default(),
        );
        assert_eq!(body["systemInstruction"], json!({ "parts": [{ "text": "You are FlashAgent." }] }));
        assert_eq!(
            body["contents"],
            json!([
                { "role": "user", "parts": [{ "text": "first" }, { "text": "second" }] },
                { "role": "model", "parts": [{ "text": "an answer" }] },
                { "role": "user", "parts": [{ "text": "thanks" }] },
            ])
        );
        for field in ["model", "messages", "stream", "extra_body", "reasoning_effort", "tools"] {
            assert!(body.get(field).is_none(), "{field} is not Gemini's");
        }
    }

    #[test]
    fn a_picture_is_sent_as_inline_data_after_the_text() {
        let mut msg = ChatMessage::user("what is wrong here?");
        msg.images = vec!["data:image/jpeg;base64,AAAA".into(), "https://example.com/not-inline.png".into()];
        let body = body("gemini-3-flash", &[msg], &[], &TurnOptions::default());
        assert_eq!(
            body["contents"][0]["parts"],
            json!([{ "text": "what is wrong here?" }, { "inlineData": { "mimeType": "image/jpeg", "data": "AAAA" } }])
        );
    }

    #[test]
    fn a_tool_call_goes_back_with_its_signature_and_its_result_with_the_function_name() {
        let replay = json!({ "protocol": "gemini", "calls": { "c1": "SIG_A" }, "text": "SIG_T", "ids": ["c1"] });
        let body = body(
            "gemini-2.5-flash",
            &[
                ChatMessage::user("look around"),
                calling("Looking.", vec![call("c1", "read_file", r#"{"path":"a.rs"}"#), call("c2", "run_shell", r#"{"command":"ls"}"#)], Some(replay)),
                ChatMessage::tool_result("c1", "fn main() {}"),
                ChatMessage::tool_result("c2", r#"{"ok": true}"#),
                ChatMessage::user("thanks"),
            ],
            &[],
            &TurnOptions::default(),
        );
        assert_eq!(
            body["contents"][1],
            json!({ "role": "model", "parts": [
                { "text": "Looking.", "thoughtSignature": "SIG_T" },
                { "functionCall": { "id": "c1", "name": "read_file", "args": { "path": "a.rs" } }, "thoughtSignature": "SIG_A" },
                { "functionCall": { "name": "run_shell", "args": { "command": "ls" } } },
            ] })
        );
        assert_eq!(
            body["contents"][2],
            json!({ "role": "user", "parts": [
                { "functionResponse": { "id": "c1", "name": "read_file", "response": { "result": "fn main() {}" } } },
                { "functionResponse": { "name": "run_shell", "response": { "result": "{\"ok\": true}" } } },
                { "text": "thanks" },
            ] }),
            "one turn for every result; an id Gemini did not give is not sent to it"
        );
    }

    #[test]
    fn a_replay_from_another_protocol_is_ignored_and_gemini_3_gets_the_placeholder_signature() {
        let foreign = json!({ "protocol": "anthropic", "calls": { "c1": "NOT_GEMINI" }, "ids": ["c1"] });
        let history = [
            ChatMessage::user("go"),
            calling("", vec![call("c1", "read_file", "{}"), call("c2", "read_file", "{}")], Some(foreign)),
            ChatMessage::tool_result("c1", "one"),
            ChatMessage::tool_result("c2", "two"),
        ];
        let body3 = body("gemini-3-flash", &history, &[], &TurnOptions::default());
        let parts = &body3["contents"][1]["parts"];
        assert_eq!(parts[0]["thoughtSignature"], UNSIGNED, "Gemini 3 refuses an unsigned call in the current turn");
        assert!(parts[1].get("thoughtSignature").is_none(), "only the first of parallel calls is signed");
        assert!(parts[0]["functionCall"].get("id").is_none());

        let body25 = body("gemini-2.5-flash", &history, &[], &TurnOptions::default());
        assert!(!body25.to_string().contains("thoughtSignature"), "2.5 is sent no signature it did not give");
    }

    #[test]
    fn broken_arguments_are_repaired_into_an_object() {
        assert_eq!(arguments(r#"{"path": "a.rs""#), json!({ "path": "a.rs" }));
        assert_eq!(arguments(r#""{\"path\":\"b.rs\"}""#), json!({ "path": "b.rs" }));
        assert_eq!(arguments("not json at all"), json!({}));
        assert_eq!(arguments(""), json!({}));
    }

    #[test]
    fn a_tool_result_without_its_call_right_before_it_is_sent_as_text() {
        let body = body(
            "gemini-3-flash",
            &[ChatMessage::system("s"), ChatMessage::tool_result("gone", "output"), ChatMessage::user("next")],
            &[],
            &TurnOptions::default(),
        );
        assert_eq!(
            body["contents"],
            json!([{ "role": "user", "parts": [{ "text": "Result of an earlier tool call:\noutput" }, { "text": "next" }] }])
        );
    }

    #[test]
    fn tool_schemas_are_sent_as_json_schema_as_written() {
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": { "mode": { "anyOf": [{ "const": "a" }, { "$ref": "#/$defs/Mode" }] }, "n": { "type": ["integer", "null"] } },
            "required": ["mode"],
            "additionalProperties": false,
            "$defs": { "Mode": { "type": "string", "enum": ["b", "c"] } },
        });
        let tools = [
            ToolSpec { name: "pick".into(), description: "Pick a mode.".into(), parameters_json: schema.to_string() },
            ToolSpec { name: "git_status".into(), description: "Status.".into(), parameters_json: r#"{"type":"object","properties":{}}"#.into() },
            ToolSpec { name: "odd".into(), description: "Broken schema.".into(), parameters_json: "{not json".into() },
        ];
        let body = body("gemini-3-flash", &[ChatMessage::user("hi")], &tools, &TurnOptions::default());
        let declarations = &body["tools"][0]["functionDeclarations"];
        let mut want = schema.clone();
        want.as_object_mut().unwrap().remove("$schema");
        assert_eq!(declarations[0], json!({ "name": "pick", "description": "Pick a mode.", "parametersJsonSchema": want }));
        assert!(declarations[0].get("parameters").is_none(), "one or the other, never both");
        assert_eq!(declarations[1], json!({ "name": "git_status", "description": "Status." }), "a tool without parameters declares none");
        assert_eq!(declarations[2], json!({ "name": "odd", "description": "Broken schema." }));
    }

    #[test]
    fn thinking_is_asked_for_in_each_family_s_own_terms() {
        let thinking = |model: &str, effort: ThinkingEffort| {
            body(model, &[ChatMessage::user("hi")], &[], &TurnOptions { thinking: effort, ..Default::default() })["generationConfig"]
                .get("thinkingConfig")
                .cloned()
        };
        assert_eq!(thinking("gemini-3-flash", ThinkingEffort::Low), Some(json!({ "thinkingLevel": "low", "includeThoughts": true })));
        assert_eq!(thinking("gemini-3-flash", ThinkingEffort::Off), Some(json!({ "thinkingLevel": "minimal", "includeThoughts": true })));
        assert_eq!(thinking("gemini-3.8-flash", ThinkingEffort::Off).unwrap()["thinkingLevel"], "low", "it refuses minimal");
        assert_eq!(thinking("gemini-3-pro-preview", ThinkingEffort::Medium).unwrap()["thinkingLevel"], "high", "it has no medium");
        assert_eq!(thinking("gemini-2.5-flash", ThinkingEffort::Off), Some(json!({ "thinkingBudget": 0, "includeThoughts": false })));
        assert_eq!(thinking("models/gemini-2.5-pro", ThinkingEffort::High).unwrap()["thinkingBudget"], 24576);
        assert_eq!(thinking("gemini-3-flash", ThinkingEffort::Default), Some(json!({ "includeThoughts": true })), "the model's own level, shown");
        assert_eq!(thinking("gemini-2.0-flash", ThinkingEffort::High), None, "it cannot think");
        assert_eq!(thinking("gemma-3-27b-it", ThinkingEffort::High), None);

        let listed = client("gemini-3-flash").with_profile(ThinkingProfile::unsupported());
        let config = request_body(&listed, &[ChatMessage::user("hi")], &[], &TurnOptions { thinking: ThinkingEffort::High, ..Default::default() });
        assert!(config.get("generationConfig").is_none(), "the listing said it does not think: {config}");

        let unlisted = client("gemini-3-flash");
        request_body(&unlisted, &[ChatMessage::user("hi")], &[], &TurnOptions::default());
        assert_eq!(unlisted.profile().unwrap().presets, vec!["minimal", "low", "medium", "high"], "the effort menu offers Gemini's levels");
    }

    #[test]
    fn only_the_sampling_fields_gemini_takes_are_sent() {
        let options = TurnOptions {
            thinking: ThinkingEffort::Low,
            temperature: Some(1.0),
            top_p: Some(0.95),
            top_k: Some(20),
            repeat_penalty: Some(1.1),
            presence_penalty: Some(0.5),
            min_p: Some(0.05),
            max_tokens: Some(4096),
            ..Default::default()
        };
        let body = body("gemini-3-flash", &[ChatMessage::user("hi")], &[], &options);
        let config = body["generationConfig"].as_object().unwrap();
        let mut keys: Vec<&str> = config.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["maxOutputTokens", "temperature", "thinkingConfig", "topK", "topP"]);
        assert_eq!(config["topK"], 20);
        assert_eq!(config["maxOutputTokens"], 4096);
        let text = body.to_string().to_lowercase();
        assert!(!text.contains("penalty") && !text.contains("min_p") && !text.contains("minp"), "{text}");
    }

    #[test]
    fn the_opening_of_the_request_stays_the_same_from_one_turn_to_the_next() {
        let tools = [ToolSpec { name: "read_file".into(), description: "Read.".into(), parameters_json: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#.into() }];
        let first = vec![ChatMessage::system("You are FlashAgent."), ChatMessage::user("Write a parser function in Rust")];
        let mut later = first.clone();
        later.push(calling("", vec![call("c1", "read_file", r#"{"path":"a.rs"}"#)], Some(json!({ "protocol": "gemini", "calls": { "c1": "S" } }))));
        later.push(ChatMessage::tool_result("c1", "fn main() {}"));
        later.push(ChatMessage::user("thanks"));
        let a = body("gemini-3-flash", &first, &tools, &TurnOptions::default());
        let b = body("gemini-3-flash", &later, &tools, &TurnOptions { thinking: ThinkingEffort::Off, ..Default::default() });
        assert_eq!(a["systemInstruction"], b["systemInstruction"]);
        assert_eq!(a["tools"], b["tools"]);
        assert_eq!(a["contents"][0], b["contents"][0]);
        assert_ne!(a["generationConfig"], b["generationConfig"], "only the settings changed");
        assert_eq!(b, body("gemini-3-flash", &later, &tools, &TurnOptions { thinking: ThinkingEffort::Off, ..Default::default() }), "the same history, the same bytes");
    }

    #[test]
    fn a_message_with_nothing_to_send_sends_no_empty_part() {
        let body = body(
            "gemini-3-flash",
            &[ChatMessage::user("hi"), ChatMessage::assistant(""), ChatMessage::user("  "), ChatMessage::assistant("hello")],
            &[],
            &TurnOptions::default(),
        );
        assert_eq!(
            body["contents"],
            json!([{ "role": "user", "parts": [{ "text": "hi" }] }, { "role": "model", "parts": [{ "text": "hello" }] }])
        );
    }

    #[test]
    fn text_and_thoughts_are_told_apart_even_when_a_chunk_is_split() {
        let stream = sse(&[
            json!({ "candidates": [{ "content": { "role": "model", "parts": [{ "text": "Weighing it up", "thought": true }] } }] }),
            json!({ "candidates": [{ "content": { "role": "model", "parts": [{ "text": "Hello" }] } }] }),
            json!({ "candidates": [{ "content": { "role": "model", "parts": [{ "text": " world" }] }, "finishReason": "STOP" }] }),
        ]);
        let (a, b) = stream.split_at(stream.len() / 2 + 7);
        let events = ok_events(decode(&[a, b]));
        assert_eq!(
            events,
            vec![
                LlmEvent::ReasoningDelta("Weighing it up".into()),
                LlmEvent::TextDelta("Hello".into()),
                LlmEvent::TextDelta(" world".into()),
                LlmEvent::Done(FinishReason::Stop),
            ]
        );
    }

    fn split_text(events: &[LlmEvent]) -> (String, String) {
        let mut reasoning = String::new();
        let mut text = String::new();
        for e in events {
            match e {
                LlmEvent::ReasoningDelta(t) => reasoning.push_str(t),
                LlmEvent::TextDelta(t) => text.push_str(t),
                _ => {}
            }
        }
        (reasoning, text)
    }

    #[test]
    fn a_thought_tag_opened_in_a_thought_and_closed_in_the_answer_is_never_shown() {
        // As gemini-3.5-flash-lite answered "hi": the thought opened a tag and
        // the answer began by closing it, so both were on screen.
        let part = |text: &str, thought: bool| json!({ "candidates": [{ "content": { "role": "model", "parts": [{ "text": text, "thought": thought }] } }] });
        let stream = sse(&[
            part("**Analyzing Request**\n\n<thought>Acknowledge and Note\n\nHi.", true),
            part("</tho", false),
            part("ught>Hello! What are we breaking or building today?", false),
            json!({ "candidates": [{ "content": { "role": "model", "parts": [{ "text": "" }] }, "finishReason": "STOP" }] }),
        ]);
        let events = ok_events(decode(&[&stream]));
        let (reasoning, text) = split_text(&events);
        assert_eq!(reasoning, "**Analyzing Request**\n\nAcknowledge and Note\n\nHi.");
        assert_eq!(text, "Hello! What are we breaking or building today?");
        assert_eq!(events.last(), Some(&LlmEvent::Done(FinishReason::Stop)));

        // A tag a thought opens and nothing closes does not swallow the answer.
        let stream = sse(&[part("<thought>Planning", true), part("The answer.", false)]);
        assert_eq!(split_text(&ok_events(decode(&[&stream]))), ("Planning".into(), "The answer.".into()));

        // Tags the answer itself carries are reasoning between them, as on the OpenAI path.
        let stream = sse(&[part("<thought>Checking</thought>Done. a < b", false)]);
        assert_eq!(split_text(&ok_events(decode(&[&stream]))), ("Checking".into(), "Done. a < b".into()));
    }

    #[test]
    fn function_calls_arrive_whole_each_with_its_own_index_and_id() {
        let stream = sse(&[json!({ "candidates": [{ "content": { "role": "model", "parts": [
            { "functionCall": { "id": "fc-1", "name": "read_file", "args": { "path": "a.rs" } }, "thoughtSignature": "SIG" },
            { "functionCall": { "name": "run_shell", "args": { "command": "ls" } } },
        ] }, "finishReason": "STOP" }] })]);
        let events = ok_events(decode(&[&stream]));
        let LlmEvent::ToolCallDelta { index: 0, id: Some(first), name: Some(n0), args_delta: a0 } = &events[0] else { panic!("{events:?}") };
        let LlmEvent::ToolCallDelta { index: 1, id: Some(second), name: Some(n1), args_delta: a1 } = &events[1] else { panic!("{events:?}") };
        assert_eq!((first.as_str(), n0.as_str(), a0.as_str()), ("fc-1", "read_file", r#"{"path":"a.rs"}"#));
        assert_eq!((n1.as_str(), a1.as_str()), ("run_shell", r#"{"command":"ls"}"#));
        assert!(!second.is_empty() && second != first, "a call Gemini did not name gets an id of its own");
        assert_eq!(events[2], LlmEvent::Replay(json!({ "protocol": "gemini", "calls": { "fc-1": "SIG" }, "ids": ["fc-1"] })));
        assert_eq!(events[3], LlmEvent::Done(FinishReason::ToolUse));
        assert_ne!(new_call_id(), new_call_id());
    }

    #[test]
    fn a_signature_on_an_empty_last_part_is_kept_for_the_next_request() {
        let stream = sse(&[
            json!({ "candidates": [{ "content": { "role": "model", "parts": [{ "text": "Done." }] } }] }),
            json!({ "candidates": [{ "content": { "role": "model", "parts": [{ "text": "", "thoughtSignature": "TEXT_SIG" }] }, "finishReason": "STOP" }] }),
        ]);
        let events = ok_events(decode(&[&stream]));
        assert_eq!(
            events,
            vec![
                LlmEvent::TextDelta("Done.".into()),
                LlmEvent::Replay(json!({ "protocol": "gemini", "text": "TEXT_SIG" })),
                LlmEvent::Done(FinishReason::Stop),
            ]
        );
    }

    #[test]
    fn a_streamed_signature_goes_back_on_the_call_it_came_with() {
        let stream = sse(&[json!({ "candidates": [{ "content": { "role": "model", "parts": [
            { "functionCall": { "name": "read_file", "args": { "path": "a.rs" } }, "thoughtSignature": "SIG_FROM_STREAM" },
        ] }, "finishReason": "STOP" }] })]);
        // As the agent loop keeps it.
        let mut reply = ChatMessage::assistant("");
        for event in ok_events(decode(&[&stream])) {
            match event {
                LlmEvent::ToolCallDelta { id, name, args_delta, .. } => reply.tool_calls.push(ToolCall { id: id.unwrap(), name: name.unwrap(), args_json: args_delta }),
                LlmEvent::Replay(state) => reply.replay = Some(state),
                _ => {}
            }
        }
        let id = reply.tool_calls[0].id.clone();
        let body = body("gemini-3-flash", &[ChatMessage::user("read a.rs"), reply, ChatMessage::tool_result(id, "fn main() {}")], &[], &TurnOptions::default());
        assert_eq!(body["contents"][1]["parts"][0]["thoughtSignature"], "SIG_FROM_STREAM");
        assert_eq!(body["contents"][2]["parts"][0]["functionResponse"]["name"], "read_file");
    }

    #[test]
    fn how_gemini_stopped_decides_how_the_turn_ends() {
        let ending = |reason: &str, message: Option<&str>| {
            let mut candidate = json!({ "content": { "role": "model", "parts": [{ "text": "partial" }] }, "finishReason": reason });
            if let Some(message) = message {
                candidate["finishMessage"] = json!(message);
            }
            decode(&[&sse(&[json!({ "candidates": [candidate] })])])
        };
        assert!(matches!(ending("MAX_TOKENS", None).last(), Some(Ok(LlmEvent::Done(FinishReason::Length)))));

        let safety = ending("SAFETY", None);
        assert!(matches!(&safety[0], Ok(LlmEvent::TextDelta(t)) if t == "partial"), "what came before is kept");
        assert!(matches!(safety.last(), Some(Err(LlmError::Forbidden(m))) if m.contains("SAFETY") && m.contains("safety filters")), "{safety:?}");

        let malformed = ending("MALFORMED_FUNCTION_CALL", Some("Malformed function call: print(default_api.run_shell())"));
        assert!(
            matches!(malformed.last(), Some(Err(LlmError::Stream(m))) if m.contains("tool call that could not be read") && m.contains("print(default_api.run_shell())")),
            "a broken call does not end the turn as if it had finished: {malformed:?}"
        );
        assert!(matches!(ending("SOMETHING_NEW", None).last(), Some(Err(LlmError::Stream(m))) if m.contains("SOMETHING_NEW")));
    }

    #[test]
    fn usage_is_reported_once_with_thoughts_counted_as_output() {
        let stream = sse(&[
            json!({ "candidates": [{ "content": { "parts": [{ "text": "a" }] } }], "usageMetadata": { "promptTokenCount": 100, "totalTokenCount": 100 } }),
            json!({
                "candidates": [{ "content": { "parts": [{ "text": "b" }] }, "finishReason": "STOP" }],
                "usageMetadata": { "promptTokenCount": 100, "candidatesTokenCount": 20, "thoughtsTokenCount": 30, "cachedContentTokenCount": 64, "totalTokenCount": 150 },
            }),
        ]);
        let usages: Vec<Usage> = ok_events(decode(&[&stream])).into_iter().filter_map(|e| if let LlmEvent::Usage(u) = e { Some(u) } else { None }).collect();
        assert_eq!(usages, vec![Usage { prompt: Some(100), completion: Some(50), cached: Some(64), mtp: None }]);
    }

    #[test]
    fn an_error_in_the_stream_ends_it() {
        let stream = sse(&[
            json!({ "candidates": [{ "content": { "parts": [{ "text": "par" }] } }] }),
            json!({ "error": { "code": 429, "message": "Resource has been exhausted (e.g. check quota).", "status": "RESOURCE_EXHAUSTED" } }),
            json!({ "candidates": [{ "content": { "parts": [{ "text": "never read" }] } }] }),
        ]);
        let items = decode(&[&stream]);
        assert!(matches!(&items[0], Ok(LlmEvent::TextDelta(t)) if t == "par"));
        assert!(matches!(&items[1], Err(LlmError::Stream(m)) if m.contains("quota") && m.contains("RESOURCE_EXHAUSTED")), "{items:?}");
        assert_eq!(items.len(), 2, "nothing after the error: {items:?}");
    }

    #[test]
    fn a_blocked_prompt_is_an_error_that_says_why() {
        let items = decode(&[&sse(&[json!({ "promptFeedback": { "blockReason": "PROHIBITED_CONTENT" }, "usageMetadata": { "promptTokenCount": 7 } })])]);
        assert!(matches!(&items[0], Ok(LlmEvent::Usage(_))));
        assert!(matches!(&items[1], Err(LlmError::Forbidden(m)) if m.contains("PROHIBITED_CONTENT") && m.contains("refused the request")), "{items:?}");
    }

    #[test]
    fn a_last_event_without_a_blank_line_is_still_read() {
        let events = ok_events(decode(&["data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"end\"}]},\"finishReason\":\"STOP\"}]}\n"]));
        assert_eq!(events, vec![LlmEvent::TextDelta("end".into()), LlmEvent::Done(FinishReason::Stop)]);
    }

    #[test]
    fn a_model_is_found_under_either_name_at_any_version_of_the_address() {
        assert_eq!(model_path("gemini-3-flash"), "models/gemini-3-flash");
        assert_eq!(model_path("models/gemini-3-flash"), "models/gemini-3-flash");
        assert_eq!(model_path("tunedModels/my-tune"), "tunedModels/my-tune");
        assert_eq!(api_root("https://generativelanguage.googleapis.com"), "https://generativelanguage.googleapis.com/v1beta");
        assert_eq!(api_root("https://generativelanguage.googleapis.com/v1alpha/"), "https://generativelanguage.googleapis.com/v1alpha");
    }

    /// Answers each request with `answer(request)` and keeps what was asked.
    async fn serve(answer: impl Fn(&str) -> (u16, String) + Send + Sync + 'static) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let request = read_request(&mut sock).await;
                log.lock().unwrap().push(request.clone());
                let (status, body) = answer(&request);
                let resp = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        (format!("http://{addr}/v1beta"), seen)
    }

    async fn read_request(sock: &mut tokio::net::TcpStream) -> String {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 16384];
        loop {
            let n = sock.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&buf).to_string();
            if let Some(end) = text.find("\r\n\r\n") {
                let length = text[..end]
                    .lines()
                    .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                    .unwrap_or(0);
                if buf.len() >= end + 4 + length {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&buf).to_string()
    }

    fn gemini_client(url: &str, model: &str) -> Client {
        Client::new(Endpoint::new(ApiProtocol::Gemini, url, Some("k".into())), model)
    }

    fn request_json(request: &str) -> Value {
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap_or_default()).unwrap_or_default()
    }

    #[tokio::test]
    async fn gemma_gets_its_tools_and_instructions_as_text_once_it_refuses_them() {
        let ok = sse(&[json!({ "candidates": [{ "content": { "role": "model", "parts": [{ "text": "ok" }] }, "finishReason": "STOP" }] })]);
        let (url, seen) = serve(move |request| {
            let body = request_json(request);
            if body.get("tools").is_some() {
                (400, r#"{"error":{"code":400,"message":"Function calling is not enabled for models/gemma-3-27b-it","status":"INVALID_ARGUMENT"}}"#.to_string())
            } else if body.get("systemInstruction").is_some() {
                (400, r#"{"error":{"code":400,"message":"Developer instruction is not enabled for models/gemma-3-27b-it","status":"INVALID_ARGUMENT"}}"#.to_string())
            } else {
                (200, ok.clone())
            }
        })
        .await;
        let llm = gemini_client(&url, "gemma-3-27b-it");
        let read_file = ToolSpec { name: "read_file".into(), description: "Read a file.".into(), parameters_json: r#"{"type":"object"}"#.into() };
        let history = [ChatMessage::system("You are FlashAgent."), ChatMessage::user("read it")];
        for _ in 0..2 {
            let events: Vec<LlmEvent> = llm.stream(&history, std::slice::from_ref(&read_file)).await.expect("answers").map(|e| e.unwrap()).collect().await;
            assert_eq!(events[0], LlmEvent::TextDelta("ok".into()));
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 4, "each refusal once, then known");
        let last = request_json(seen.last().unwrap());
        let prompt = last["contents"][0]["parts"][0]["text"].as_str().unwrap();
        assert!(prompt.starts_with("You are FlashAgent.\n\n# Tools") && prompt.contains("## read_file") && prompt.ends_with("read it"), "{prompt}");
    }

    #[test]
    fn a_model_listed_without_tools_is_never_sent_declarations() {
        let llm = client("gemma-3-27b-it");
        let mut gemma = DiscoveredModel { id: "gemma-3-27b-it".into(), display_name: None, is_loaded: false, context_length: None, max_context_length: None, thinking: ThinkingProfile::unsupported(), supports_tools: false, supports_vision: false };
        llm.settle_discovery(0, vec![gemma.clone()], ServerKind::Gemini, None);
        let read_file = ToolSpec { name: "read_file".into(), description: "Read a file.".into(), parameters_json: r#"{"type":"object"}"#.into() };
        let sent = request_body(&llm, &[ChatMessage::system("Be brief."), ChatMessage::user("hi")], std::slice::from_ref(&read_file), &TurnOptions::default());
        assert!(sent.get("tools").is_none());
        assert!(sent["systemInstruction"]["parts"][0]["text"].as_str().unwrap().contains("## read_file"));

        gemma.id = "gemini-3-flash".into();
        gemma.supports_tools = true;
        llm.set_model("gemini-3-flash");
        llm.settle_discovery(0, vec![gemma], ServerKind::Gemini, None);
        let sent = request_body(&llm, &[ChatMessage::user("hi")], &[read_file], &TurnOptions::default());
        assert!(sent.get("tools").is_some(), "Gemini itself takes declarations");
    }

    #[tokio::test]
    async fn a_turn_goes_to_the_native_endpoint_with_the_key_in_a_header() {
        let answer = sse(&[
            json!({ "candidates": [{ "content": { "role": "model", "parts": [{ "text": "Hello" }] } }] }),
            json!({ "candidates": [{ "content": { "role": "model", "parts": [{ "text": "" }] }, "finishReason": "STOP" }], "usageMetadata": { "promptTokenCount": 5, "candidatesTokenCount": 1 } }),
        ]);
        let (url, seen) = serve(move |_| (200, answer.clone())).await;
        for model in ["models/gemini-3-flash", "gemini-3-flash"] {
            let llm = gemini_client(&url, model);
            let mut stream = llm.stream_with_options(&[ChatMessage::user("hi")], &[], &TurnOptions::default()).await.expect("streams");
            let mut events = Vec::new();
            while let Some(item) = stream.next().await {
                events.push(item.expect("no error"));
            }
            assert_eq!(
                events,
                vec![
                    LlmEvent::TextDelta("Hello".into()),
                    LlmEvent::Usage(Usage { prompt: Some(5), completion: Some(1), cached: None, mtp: None }),
                    LlmEvent::Done(FinishReason::Stop),
                ]
            );
            assert_eq!(llm.requests_in_flight(), 0);
        }
        for request in seen.lock().unwrap().iter() {
            let first_line = request.lines().next().unwrap();
            assert_eq!(first_line, "POST /v1beta/models/gemini-3-flash:streamGenerateContent?alt=sse HTTP/1.1", "either name, one path");
            assert!(request.to_ascii_lowercase().contains("\r\nx-goog-api-key: k\r\n"), "{request}");
            assert!(!first_line.contains("key="), "the key never goes in the address");
            assert!(request.contains(r#""contents":[{"parts":[{"text":"hi"}],"role":"user"}]"#), "{request}");
        }
    }

    #[tokio::test]
    async fn a_thinking_level_the_model_refuses_is_dropped_and_not_offered_again() {
        let ok = sse(&[json!({ "candidates": [{ "content": { "parts": [{ "text": "ok" }] }, "finishReason": "STOP" }] })]);
        let (url, seen) = serve(move |request| {
            if request.contains(r#""thinkingLevel":"minimal""#) {
                (400, r#"{"error":{"code":400,"message":"Thinking level MINIMAL is not supported for this model.","status":"INVALID_ARGUMENT"}}"#.to_string())
            } else {
                (200, ok.clone())
            }
        })
        .await;
        let llm = gemini_client(&url, "gemini-3-flash");
        let off = TurnOptions { thinking: ThinkingEffort::Off, ..Default::default() };
        for _ in 0..2 {
            let mut stream = llm.stream_with_options(&[ChatMessage::user("hi")], &[], &off).await.expect("asked again without it");
            while stream.next().await.is_some() {}
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3, "one refusal, then none");
        assert!(seen[1].contains(r#""thinkingConfig":{"includeThoughts":true}"#), "{}", seen[1]);
        assert!(seen[2].contains(r#""thinkingLevel":"low""#), "{}", seen[2]);
        assert!(!llm.profile().unwrap().presets.contains(&"minimal".to_string()));
    }

    #[tokio::test]
    async fn an_error_status_comes_back_with_its_body() {
        let (url, seen) = serve(|_| (404, r#"{"error":{"code":404,"message":"models/gemini-9 is not found for API version v1beta"}}"#.to_string())).await;
        let llm = gemini_client(&url, "gemini-9");
        let result = llm.stream_with_options(&[ChatMessage::user("hi")], &[], &TurnOptions::default()).await;
        assert!(matches!(result, Err(LlmError::Status { status: 404, ref body }) if body.contains("is not found")));
        assert_eq!(seen.lock().unwrap().len(), 1, "a refusal that is not about thinking is not asked again");
        assert_eq!(llm.requests_in_flight(), 0);
    }

    #[tokio::test]
    async fn discovery_reads_every_page_and_names_models_the_short_way() {
        let (url, seen) = serve(|request| {
            let page = if request.contains("pageToken=") {
                json!({ "models": [
                    { "name": "models/gemini-3-flash", "displayName": "Gemini 3 Flash", "inputTokenLimit": 1048576, "thinking": true,
                      "supportedGenerationMethods": ["generateContent", "countTokens"] },
                    { "name": "models/gemma-3-27b-it", "inputTokenLimit": 131072, "supportedGenerationMethods": ["generateContent"] },
                    { "name": "models/text-embedding-004", "inputTokenLimit": 2048, "supportedGenerationMethods": ["embedContent"] },
                ] })
            } else {
                json!({ "models": [
                    { "name": "models/gemini-2.5-flash", "inputTokenLimit": 1048576, "thinking": true, "supportedGenerationMethods": ["generateContent"] },
                    { "name": "models/gemini-2.5-flash-preview-tts", "inputTokenLimit": 8192, "supportedGenerationMethods": ["generateContent"] },
                ], "nextPageToken": "p2+/=" })
            };
            (200, page.to_string())
        })
        .await;
        let llm = gemini_client(&url, "models/gemini-3-flash");
        let disc = llm.discover_server().await.expect("listed");
        let ids: Vec<&str> = disc.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["gemini-2.5-flash", "gemini-3-flash", "gemma-3-27b-it"], "chat models only, named the short way");
        assert_eq!(disc.kind, ServerKind::Gemini);
        assert_eq!(llm.model(), "gemini-3-flash", "the model saved under its long name is found");
        let flash = &disc.models[1];
        assert_eq!(flash.context_length, Some(1048576));
        assert_eq!(flash.display_name.as_deref(), Some("Gemini 3 Flash"));
        assert!(flash.supports_tools && flash.supports_vision);
        assert_eq!(flash.thinking.presets, vec!["minimal", "low", "medium", "high"]);
        assert_eq!(llm.profile().unwrap().presets, flash.thinking.presets);
        assert!(!disc.models[2].supports_tools && !disc.models[2].thinking.supported, "Gemma takes no tools and does not think");

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen[0].starts_with("GET /v1beta/models?pageSize=1000 "), "{}", seen[0]);
        assert!(seen[1].starts_with("GET /v1beta/models?pageSize=1000&pageToken=p2%2B%2F%3D "), "{}", seen[1]);
        assert!(seen.iter().all(|r| r.to_ascii_lowercase().contains("x-goog-api-key: k")));
    }

    #[tokio::test]
    async fn discovery_without_a_key_asks_nothing() {
        let (url, seen) = serve(|_| (200, r#"{"models":[]}"#.to_string())).await;
        let llm = Client::new(Endpoint::new(ApiProtocol::Gemini, url, None), "gemini-3-flash");
        assert!(llm.discover_server().await.is_none());
        assert!(seen.lock().unwrap().is_empty());
    }
}
