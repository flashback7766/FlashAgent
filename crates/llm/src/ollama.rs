//! Ollama's own API (`/api/chat`, `/api/tags`, `/api/ps`, `/api/show`).
//!
//! What it has over Ollama's OpenAI copy at `/v1` is `num_ctx`. Ollama loads
//! a model with a small context unless told otherwise and cuts a longer
//! prompt from the front without a word; the system prompt and the tools
//! alone can fill it. So every request names the context, chosen once per
//! model from what the server reports and kept the same from turn to turn:
//! a different `num_ctx` makes Ollama load the model again.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::Duration;

use futures::StreamExt;
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::client::{pump, Client, EventStream, WireDecoder};
use crate::repair::repair_json;
use crate::thinking::{DiscoveredModel, ServerDiscovery, ServerKind, ThinkingProfile, ThinkingProtocol};
use crate::types::{ChatMessage, FinishReason, LlmError, LlmEvent, Role, ToolSpec, TurnOptions, Usage};

/// The context asked for when the server shows no better choice: room for the
/// system prompt, the tools and a working conversation, and a KV cache that
/// still fits beside an 8B model's weights on a 12–16 GB GPU (about 4.5 GB
/// at f16). Never more than the model was trained for.
const DEFAULT_NUM_CTX: usize = 32_768;
/// A model already loaded with at least this much keeps it: loading it again
/// takes seconds and empties its prompt cache, and whoever loaded it chose
/// the size (`OLLAMA_CONTEXT_LENGTH`, a Modelfile, Ollama sizing it for the
/// GPU). Less cannot hold the agent's prompt, so it is replaced.
const MIN_NUM_CTX: usize = 16_384;
/// Ollama unloads a model 5 minutes after its last request, and its prompt
/// cache with it: a user reading an answer and writing the next message
/// often takes longer. 30 minutes keeps a working session warm and still
/// frees the memory of one that was left.
const KEEP_ALIVE: &str = "30m";
/// `/api/show` for each installed model, a few at a time.
const SHOW_CONCURRENCY: usize = 4;

/// The server's root: the address may be given with `/api` (or `/v1`) after it.
fn root(client: &Client) -> String {
    let base = client.base_url();
    let base = base.trim_end_matches('/');
    base.strip_suffix("/api").or_else(|| base.strip_suffix("/v1")).unwrap_or(base).to_string()
}

pub(crate) async fn stream(
    client: &Client,
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    options: &TurnOptions,
) -> Result<EventStream, LlmError> {
    let busy = client.busy();
    let url = format!("{}/api/chat", root(client));
    let headers = crate::openai::headers(client);
    let mut body = body(client, messages, tools, options);
    let mut resp = client.post(&url, &headers, &body).await?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        // A model that cannot think refuses `think` outright. Known from then on.
        let refused_thinking = status == 400 && text.contains("does not support thinking");
        if !refused_thinking || body.as_object_mut().and_then(|b| b.remove("think")).is_none() {
            return Err(LlmError::Status { status, body: text });
        }
        client.set_profile(no_thinking());
        resp = client.post(&url, &headers, &body).await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            return Err(LlmError::Status { status, body: resp.text().await.unwrap_or_default() });
        }
    }
    Ok(pump(resp, busy, Decoder::default()))
}

fn no_thinking() -> ThinkingProfile {
    ThinkingProfile { protocol: ThinkingProtocol::Ollama, ..ThinkingProfile::unsupported() }
}

fn body(client: &Client, messages: &[ChatMessage], tools: &[ToolSpec], options: &TurnOptions) -> Value {
    let model = client.model();
    let mut opts = serde_json::Map::new();
    opts.insert("num_ctx".into(), json!(num_ctx(client, &model)));
    let mut set = |key: &str, value: Option<Value>| {
        if let Some(value) = value {
            opts.insert(key.into(), value);
        }
    };
    set("temperature", options.temperature.map(Value::from));
    set("top_p", options.top_p.map(Value::from));
    set("top_k", options.top_k.map(Value::from));
    set("repeat_penalty", options.repeat_penalty.map(Value::from));
    set("presence_penalty", options.presence_penalty.map(Value::from));
    set("min_p", options.min_p.map(Value::from));
    set("num_predict", options.max_tokens.map(Value::from));

    let mut body = json!({
        "model": model,
        "messages": messages_json(messages),
        "stream": true,
        "keep_alive": KEEP_ALIVE,
        "options": opts,
    });
    if !tools.is_empty() {
        body["tools"] = crate::openai::tools_json(tools);
    }
    if let Some(think) = think(client, messages, options) {
        body["think"] = think;
    }
    body
}

/// The context chosen for the model when the server was looked at; the same
/// on every request, since a change reloads the model.
fn num_ctx(client: &Client, model: &str) -> usize {
    client
        .discovery()
        .and_then(|d| d.models.into_iter().chain(d.active_model).find(|m| m.id == model))
        .and_then(|m| m.context_length)
        .unwrap_or(DEFAULT_NUM_CTX)
}

/// `true`/`false`, or a level for a model that takes levels (gpt-oss). Left
/// out when the model's default is wanted or the model cannot think: such a
/// model refuses `think: true`.
fn think(client: &Client, messages: &[ChatMessage], options: &TurnOptions) -> Option<Value> {
    let profile = client.profile().filter(|p| p.protocol == ThinkingProtocol::Ollama && p.supported)?;
    let effort = client.resolve_effort(messages, options)?.to_ascii_lowercase();
    let is_level = |p: &&String| **p == effort && !matches!(p.as_str(), "on" | "off");
    Some(match profile.presets.iter().find(is_level) {
        Some(level) => json!(level),
        None => json!(!matches!(effort.as_str(), "off" | "none" | "disabled" | "false" | "0")),
    })
}

fn messages_json(messages: &[ChatMessage]) -> Vec<Value> {
    // A tool result names its tool, looked up from the call it answers.
    let mut tool_names: HashMap<&str, &str> = HashMap::new();
    messages
        .iter()
        .map(|m| {
            let mut v = json!({ "role": m.role.as_str(), "content": m.content });
            match m.role {
                Role::Assistant => {
                    if let Some(reasoning) = m.reasoning.as_deref().filter(|r| !r.trim().is_empty()) {
                        v["thinking"] = json!(reasoning);
                    }
                    if !m.tool_calls.is_empty() {
                        let calls = m
                            .tool_calls
                            .iter()
                            .map(|c| {
                                tool_names.insert(&c.id, &c.name);
                                json!({ "id": c.id, "function": { "name": c.name, "arguments": arguments(&c.args_json) } })
                            })
                            .collect();
                        v["tool_calls"] = Value::Array(calls);
                    }
                }
                Role::Tool => {
                    if let Some(id) = m.tool_call_id.as_deref() {
                        v["tool_call_id"] = json!(id);
                        if let Some(name) = tool_names.get(id) {
                            v["tool_name"] = json!(name);
                        }
                    }
                }
                Role::System | Role::User => {}
            }
            if !m.images.is_empty() {
                v["images"] = m.images.iter().map(|url| json!(bare_base64(url))).collect();
            }
            v
        })
        .collect()
}

/// Ollama takes arguments as an object, not as JSON text.
fn arguments(args_json: &str) -> Value {
    repair_json(args_json)
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

/// Ollama takes the base64 alone, without the `data:…;base64,` in front.
fn bare_base64(url: &str) -> &str {
    match url.strip_prefix("data:").and_then(|rest| rest.split_once(',')) {
        Some((_, data)) => data,
        None => url,
    }
}

/// Newline-delimited JSON: one object per line, the last with `done: true`.
#[derive(Default)]
struct Decoder {
    /// An unfinished line.
    partial: Vec<u8>,
    /// Calls so far: each arrives whole and takes the next index.
    calls: usize,
    finished: bool,
}

impl Decoder {
    fn line(&mut self, line: &[u8], out: &mut Vec<Result<LlmEvent, LlmError>>) {
        let line = line.trim_ascii();
        if line.is_empty() || self.finished {
            return;
        }
        let Ok(mut v) = serde_json::from_slice::<Value>(line) else { return };
        // Failures after the 200 (out of memory, a crashed runner) arrive as a line.
        if let Some(msg) = crate::openai::stream_error(&v) {
            self.finished = true;
            out.push(Err(LlmError::Stream(msg)));
            return;
        }
        if let Some(message) = v.get_mut("message") {
            let mut take_text = |key: &str| match message.get_mut(key).map(Value::take) {
                Some(Value::String(s)) if !s.is_empty() => Some(s),
                _ => None,
            };
            if let Some(thinking) = take_text("thinking") {
                out.push(Ok(LlmEvent::ReasoningDelta(thinking)));
            }
            if let Some(content) = take_text("content") {
                out.push(Ok(LlmEvent::TextDelta(content)));
            }
            if let Some(Value::Array(calls)) = message.get_mut("tool_calls").map(Value::take) {
                for call in calls {
                    out.push(Ok(self.tool_call(call)));
                }
            }
        }
        if v.get("done").and_then(Value::as_bool) == Some(true) {
            self.finished = true;
            let count = |key: &str| v.get(key).and_then(Value::as_i64);
            let (prompt, completion) = (count("prompt_eval_count"), count("eval_count"));
            if prompt.is_some() || completion.is_some() {
                out.push(Ok(LlmEvent::Usage(Usage { prompt, completion, cached: count("prompt_eval_cached_count"), mtp: None })));
            }
            let reason = if self.calls > 0 {
                FinishReason::ToolUse
            } else if v.get("done_reason").and_then(Value::as_str) == Some("length") {
                FinishReason::Length
            } else {
                FinishReason::Stop
            };
            out.push(Ok(LlmEvent::Done(reason)));
        }
    }

    fn tool_call(&mut self, mut call: Value) -> LlmEvent {
        let mut function = call.get_mut("function").map(Value::take).unwrap_or(Value::Null);
        let id = match call.get_mut("id").map(Value::take) {
            Some(Value::String(id)) if !id.is_empty() => id,
            // Before 0.12 Ollama gave calls no id; results are matched by it.
            _ => new_call_id(),
        };
        let name = match function.get_mut("name").map(Value::take) {
            Some(Value::String(name)) => name,
            _ => String::new(),
        };
        let args_delta = match function.get_mut("arguments").map(Value::take) {
            Some(Value::String(text)) => text,
            Some(Value::Null) | None => "{}".to_string(),
            Some(object) => object.to_string(),
        };
        let index = self.calls;
        self.calls += 1;
        LlmEvent::ToolCallDelta { index, id: Some(id), name: Some(name), args_delta }
    }
}

impl WireDecoder for Decoder {
    fn feed(&mut self, bytes: &[u8]) -> Vec<Result<LlmEvent, LlmError>> {
        let mut out = Vec::new();
        // Only the new bytes are searched; a line cut between chunks waits in `partial`.
        let Some(last_newline) = bytes.iter().rposition(|&b| b == b'\n') else {
            self.partial.extend_from_slice(bytes);
            return out;
        };
        let (complete, rest) = bytes.split_at(last_newline + 1);
        let mut lines = complete.split(|&b| b == b'\n');
        if !self.partial.is_empty() {
            let mut first = std::mem::take(&mut self.partial);
            first.extend_from_slice(lines.next().unwrap_or_default());
            self.line(&first, &mut out);
        }
        for line in lines {
            self.line(line, &mut out);
        }
        self.partial.extend_from_slice(rest);
        out
    }

    fn finish(&mut self) -> Vec<Result<LlmEvent, LlmError>> {
        let mut out = Vec::new();
        let rest = std::mem::take(&mut self.partial);
        self.line(&rest, &mut out);
        out
    }
}

/// Nine letters and digits, unique for the life of the process: some chat
/// templates (Mistral's) insist on that shape if the session moves there.
fn new_call_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
    let mut n = seed.rotate_left(23) ^ NEXT.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    (0..9)
        .map(|_| {
            let c = ALPHABET[(n % ALPHABET.len() as u64) as usize] as char;
            n /= ALPHABET.len() as u64;
            c
        })
        .collect()
}

/// What `/api/show` says about a model. It never changes for one digest, so
/// it is asked once per model and kept for the life of the process.
#[derive(Debug, Clone, PartialEq)]
struct Shown {
    chat: bool,
    tools: bool,
    vision: bool,
    thinking: ThinkingProfile,
    /// What the model was trained for.
    max_context: Option<usize>,
    /// `PARAMETER num_ctx` in its Modelfile.
    modelfile_num_ctx: Option<usize>,
}

static SHOWN: LazyLock<Mutex<HashMap<String, Shown>>> = LazyLock::new(Default::default);

impl Shown {
    fn parse(name: &str, v: &Value) -> Self {
        let capabilities: Option<Vec<&str>> = v.get("capabilities").and_then(Value::as_array).map(|c| c.iter().filter_map(Value::as_str).collect());
        let info = v.get("model_info");
        let arch = info.and_then(|i| i.get("general.architecture")).and_then(Value::as_str);
        let max_context = arch
            .and_then(|arch| info?.get(format!("{arch}.context_length")))
            .or_else(|| info?.as_object()?.iter().find(|(k, _)| k.ends_with(".context_length")).map(|(_, v)| v))
            .and_then(Value::as_u64)
            .map(|n| n as usize);
        let modelfile_num_ctx = v.get("parameters").and_then(Value::as_str).and_then(|params| {
            params.lines().find_map(|line| {
                let mut words = line.split_whitespace();
                (words.next() == Some("num_ctx")).then(|| words.next()?.parse().ok()).flatten()
            })
        });
        let family = v.pointer("/details/family").and_then(Value::as_str).unwrap_or_default();
        match capabilities {
            Some(caps) => {
                let has = |c: &str| caps.contains(&c);
                Shown {
                    // Embedding and image models cannot chat.
                    chat: caps.is_empty() || has("completion"),
                    tools: has("tools"),
                    vision: has("vision"),
                    thinking: thinking_profile(name, family, v, has("thinking")),
                    max_context,
                    modelfile_num_ctx,
                }
            }
            // Before 0.6.4 the server did not say; the template and the projector do.
            None => Shown {
                chat: !family.contains("bert"),
                tools: v.get("template").and_then(Value::as_str).is_some_and(|t| t.contains(".Tools")),
                vision: v.get("projector_info").and_then(Value::as_object).is_some_and(|p| !p.is_empty()),
                thinking: ThinkingProfile::unreported(),
                max_context,
                modelfile_num_ctx,
            },
        }
    }
}

/// The server's own list of `think` values where it gives one; otherwise
/// on/off, or gpt-oss's levels (it cannot stop thinking).
fn thinking_profile(name: &str, family: &str, show: &Value, thinks: bool) -> ThinkingProfile {
    let value_name = |x: &Value| match x {
        Value::Bool(true) => Some("on".to_string()),
        Value::Bool(false) => Some("off".to_string()),
        Value::String(s) if !s.is_empty() => Some(s.to_ascii_lowercase()),
        _ => None,
    };
    if let Some(values) = show.pointer("/thinking/values").and_then(Value::as_array) {
        let mut presets: Vec<String> = Vec::new();
        for preset in values.iter().filter_map(value_name) {
            if !presets.contains(&preset) {
                presets.push(preset);
            }
        }
        if !presets.is_empty() {
            let default_preset = show.pointer("/thinking/default").and_then(value_name);
            return ThinkingProfile { presets, protocol: ThinkingProtocol::Ollama, supported: true, default_preset };
        }
    }
    if !thinks {
        return no_thinking();
    }
    let levels = family.eq_ignore_ascii_case("gptoss") || name.contains("gpt-oss");
    let (presets, default): (&[&str], &str) = if levels { (&["low", "medium", "high"], "medium") } else { (&["off", "on"], "on") };
    ThinkingProfile {
        presets: presets.iter().map(|p| p.to_string()).collect(),
        protocol: ThinkingProtocol::Ollama,
        supported: true,
        default_preset: Some(default.to_string()),
    }
}

/// The context a model runs with; see [`MIN_NUM_CTX`] and [`DEFAULT_NUM_CTX`].
/// What was chosen `earlier` in this session is kept, loaded or not: every
/// request sends it, so the model is loaded with it, and a later reading of
/// `/api/ps` cannot move it (where Ollama counts the context of all parallel
/// slots together, following it would grow the context at every look).
fn choose_num_ctx(earlier: Option<usize>, loaded: Option<usize>, modelfile: Option<usize>, max: Option<usize>) -> usize {
    let chosen = earlier
        .or_else(|| [loaded, modelfile].into_iter().flatten().find(|&n| n >= MIN_NUM_CTX))
        .unwrap_or(DEFAULT_NUM_CTX);
    match max.filter(|&m| m > 0) {
        Some(max) => chosen.min(max),
        None => chosen,
    }
}

async fn show(client: &Client, root: &str, headers: &reqwest::header::HeaderMap, name: &str, digest: &str) -> Option<Shown> {
    let key = format!("{root}\n{name}\n{digest}");
    if let Some(known) = SHOWN.lock().get(&key) {
        return Some(known.clone());
    }
    let resp = client
        .http
        .post(format!("{root}/api/show"))
        .headers(headers.clone())
        // `name` for servers older than `model`.
        .json(&json!({ "model": name, "name": name }))
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let shown = Shown::parse(name, &resp.json::<Value>().await.ok()?);
    if !digest.is_empty() {
        SHOWN.lock().insert(key, shown.clone());
    }
    Some(shown)
}

/// The installed models (`/api/tags`), which are loaded and with what context
/// (`/api/ps`), and what each can do (`/api/show`).
pub(crate) async fn discover(client: &Client) -> Option<ServerDiscovery> {
    let root = root(client);
    let headers = crate::openai::headers(client);
    let timeout = Duration::from_secs(2);
    let (tags_url, ps_url) = (format!("{root}/api/tags"), format!("{root}/api/ps"));
    let (tags, ps) = futures::join!(client.get_json(&tags_url, &headers, timeout), client.get_json(&ps_url, &headers, timeout));
    let tags = tags?;
    let installed: Vec<(String, String)> = tags
        .get("models")?
        .as_array()?
        .iter()
        .filter_map(|m| {
            let name = m.get("name").or_else(|| m.get("model")).and_then(Value::as_str)?.trim();
            let digest = m.get("digest").and_then(Value::as_str).unwrap_or_default();
            (!name.is_empty()).then(|| (name.to_string(), digest.to_string()))
        })
        .collect();
    // Name → the context it is loaded with (0 or absent before 0.11: unknown).
    let loaded: HashMap<String, Option<usize>> = ps
        .as_ref()
        .and_then(|ps| ps.get("models")?.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|m| {
            let context = m.get("context_length").and_then(Value::as_u64).filter(|&n| n > 0).map(|n| n as usize);
            let name = m.get("name").or_else(|| m.get("model")).and_then(Value::as_str)?;
            Some((name.to_string(), context))
        })
        .collect();
    let earlier = client.discovery();
    let earlier_ctx = |name: &str| earlier.as_ref()?.models.iter().find(|m| m.id == name)?.context_length;

    // Owned names: a closure over borrowed ones makes the future not provably `Send`.
    let (root, headers) = (&root, &headers);
    let shown: Vec<Option<Shown>> = futures::stream::iter(installed.clone())
        .map(|(name, digest)| async move { show(client, root, headers, &name, &digest).await })
        .buffered(SHOW_CONCURRENCY)
        .collect()
        .await;

    let models: Vec<DiscoveredModel> = installed
        .into_iter()
        .zip(shown)
        .filter_map(|((name, _), shown)| {
            // A model `/api/show` did not answer for is still offered, its abilities unknown.
            let shown = shown.unwrap_or(Shown {
                chat: true,
                tools: true,
                vision: false,
                thinking: ThinkingProfile::unreported(),
                max_context: None,
                modelfile_num_ctx: None,
            });
            if !shown.chat {
                return None;
            }
            let loaded_ctx = loaded.get(&name);
            let context = choose_num_ctx(earlier_ctx(&name), loaded_ctx.copied().flatten(), shown.modelfile_num_ctx, shown.max_context);
            Some(DiscoveredModel {
                is_loaded: loaded_ctx.is_some(),
                context_length: Some(context),
                max_context_length: shown.max_context,
                thinking: shown.thinking,
                supports_tools: shown.tools,
                supports_vision: shown.vision,
                display_name: None,
                id: name,
            })
        })
        .collect();
    // Ollama loads any installed model on demand, so the model the user chose
    // stays chosen; preferring a loaded one would undo a pick at the next look.
    let current = client.model();
    let chosen = models.iter().find(|m| m.id == current || m.id == format!("{current}:latest")).cloned();
    let mut disc = client.settle_discovery(models, ServerKind::Ollama)?;
    if let Some(chosen) = chosen.filter(|c| disc.active_model.as_ref().is_none_or(|a| a.id != c.id)) {
        client.set_model(&chosen.id);
        disc.active_model = Some(chosen);
        client.adopt_discovery(&disc);
    }
    Some(disc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai::test_server;
    use crate::protocol::{ApiProtocol, Endpoint};
    use crate::types::{ThinkingEffort, ToolCall};
    use crate::LlmBackend;

    fn client(url: &str, model: &str) -> Client {
        Client::new(Endpoint::new(ApiProtocol::Ollama, url, None), model)
    }

    fn profile(presets: &[&str], default: &str) -> ThinkingProfile {
        ThinkingProfile {
            presets: presets.iter().map(|p| p.to_string()).collect(),
            protocol: ThinkingProtocol::Ollama,
            supported: true,
            default_preset: Some(default.to_string()),
        }
    }

    fn model(id: &str, context: usize) -> DiscoveredModel {
        DiscoveredModel {
            id: id.into(),
            display_name: None,
            is_loaded: true,
            context_length: Some(context),
            max_context_length: Some(131_072),
            thinking: profile(&["off", "on"], "on"),
            supports_tools: true,
            supports_vision: true,
        }
    }

    fn decode(pieces: &[&[u8]]) -> Vec<Result<LlmEvent, LlmError>> {
        let mut d = Decoder::default();
        let mut out = Vec::new();
        for piece in pieces {
            out.extend(d.feed(piece));
        }
        out.extend(d.finish());
        out
    }

    fn ok(events: Vec<Result<LlmEvent, LlmError>>) -> Vec<LlmEvent> {
        events.into_iter().map(|e| e.expect("no error")).collect()
    }

    #[test]
    fn a_turn_is_sent_the_way_ollama_reads_it() {
        let llm = client("http://localhost:11434", "qwen3:8b");
        llm.settle_discovery(vec![model("qwen3:8b", 40_960)], ServerKind::Ollama);
        let mut user = ChatMessage::user("what is on screen?");
        user.images.push("data:image/png;base64,iVBORw0KGgo=".into());
        let mut assistant = ChatMessage::assistant("");
        assistant.reasoning = Some("look at the file first".into());
        assistant.tool_calls = vec![
            ToolCall { id: "c1".into(), name: "read_file".into(), args_json: r#"{"path": "a.rs",}"#.into() },
            ToolCall { id: "c2".into(), name: "grep".into(), args_json: "not json".into() },
        ];
        let messages = [
            ChatMessage::system("You are FlashAgent."),
            user,
            assistant,
            ChatMessage::tool_result("c1", "fn main() {}"),
            ChatMessage::tool_result("c2", "no matches"),
        ];
        let tools = [ToolSpec { name: "read_file".into(), description: "read".into(), parameters_json: r#"{"type":"object"}"#.into() }];
        let options = TurnOptions {
            temperature: Some(0.5),
            top_p: Some(0.95),
            top_k: Some(20),
            repeat_penalty: Some(1.0),
            presence_penalty: Some(0.0),
            min_p: Some(0.0),
            max_tokens: Some(512),
            ..Default::default()
        };
        let body = body(&llm, &messages, &tools, &options);
        assert_eq!(body["model"], "qwen3:8b");
        assert_eq!(body["stream"], true);
        assert_eq!(body["keep_alive"], KEEP_ALIVE);
        let m = &body["messages"];
        assert_eq!(m[0], json!({ "role": "system", "content": "You are FlashAgent." }));
        assert_eq!(m[1]["images"], json!(["iVBORw0KGgo="]), "base64 alone, without the data: prefix");
        assert_eq!(m[2]["thinking"], "look at the file first");
        assert_eq!(m[2]["tool_calls"][0], json!({ "id": "c1", "function": { "name": "read_file", "arguments": { "path": "a.rs" } } }), "arguments are an object, repaired");
        assert_eq!(m[2]["tool_calls"][1]["function"]["arguments"], json!({}));
        assert_eq!(m[3], json!({ "role": "tool", "content": "fn main() {}", "tool_call_id": "c1", "tool_name": "read_file" }));
        assert_eq!(m[4]["tool_name"], "grep");
        assert_eq!(body["tools"][0]["function"]["name"], "read_file");
        assert_eq!(body["tools"][0]["type"], "function");
        let o = &body["options"];
        assert_eq!(o["num_ctx"], 40_960, "the context chosen when the server was looked at");
        assert_eq!(o["num_predict"], 512);
        assert_eq!(o["top_k"], 20);
        for key in ["temperature", "top_p", "repeat_penalty", "presence_penalty", "min_p"] {
            assert!(o[key].is_number(), "{key}");
        }
        // The same context on every turn: another one reloads the model.
        let later = self::body(&llm, &[ChatMessage::user("hi")], &[], &TurnOptions::default());
        assert_eq!(later["options"], json!({ "num_ctx": 40_960 }), "nothing unset is sent");
        assert!(later.get("tools").is_none());
    }

    #[test]
    fn a_model_that_was_never_looked_up_gets_the_default_context() {
        let llm = client("http://localhost:11434", "unknown:latest");
        assert_eq!(body(&llm, &[ChatMessage::user("hi")], &[], &TurnOptions::default())["options"]["num_ctx"], DEFAULT_NUM_CTX);
    }

    #[test]
    fn think_is_a_switch_a_level_or_left_out() {
        let think_for = |p: Option<ThinkingProfile>, options: TurnOptions| {
            let mut llm = client("http://localhost:11434", "m");
            if let Some(p) = p {
                llm = llm.with_profile(p);
            }
            body(&llm, &[ChatMessage::user("Write a parser for this format in Rust")], &[], &options).get("think").cloned()
        };
        let effort = |thinking| TurnOptions { thinking, ..Default::default() };
        // Unknown or unable: never sent, since a model that cannot think refuses `think: true`.
        assert_eq!(think_for(None, effort(ThinkingEffort::High)), None);
        assert_eq!(think_for(Some(no_thinking()), effort(ThinkingEffort::Off)), None);
        let switch = profile(&["off", "on"], "on");
        assert_eq!(think_for(Some(switch.clone()), effort(ThinkingEffort::Off)), Some(json!(false)));
        assert_eq!(think_for(Some(switch.clone()), effort(ThinkingEffort::High)), Some(json!(true)));
        assert_eq!(think_for(Some(switch.clone()), effort(ThinkingEffort::Auto)), Some(json!(true)), "code is worth thinking about");
        let custom = TurnOptions { custom_effort: Some("high".into()), ..Default::default() };
        assert_eq!(think_for(Some(switch), custom), Some(json!(true)), "a level the model does not take is just on");
        let levels = profile(&["low", "medium", "high"], "medium");
        assert_eq!(think_for(Some(levels.clone()), effort(ThinkingEffort::High)), Some(json!("high")));
        assert_eq!(think_for(Some(levels.clone()), effort(ThinkingEffort::Off)), Some(json!("low")), "gpt-oss cannot stop thinking");
        assert_eq!(think_for(Some(levels), effort(ThinkingEffort::Default)), Some(json!("medium")));
    }

    #[test]
    fn the_context_is_what_the_server_runs_else_a_default_never_above_the_model() {
        assert_eq!(choose_num_ctx(None, None, None, Some(131_072)), DEFAULT_NUM_CTX);
        assert_eq!(choose_num_ctx(None, None, None, None), DEFAULT_NUM_CTX);
        assert_eq!(choose_num_ctx(None, None, None, Some(8192)), 8192, "an old model's own limit");
        assert_eq!(choose_num_ctx(None, Some(65_536), None, Some(131_072)), 65_536, "loaded large enough: no reload");
        assert_eq!(choose_num_ctx(None, Some(4096), None, Some(131_072)), DEFAULT_NUM_CTX, "Ollama's small default cannot hold the prompt");
        assert_eq!(choose_num_ctx(None, None, Some(24_576), Some(131_072)), 24_576, "the Modelfile's own setting");
        assert_eq!(choose_num_ctx(None, None, Some(2048), Some(131_072)), DEFAULT_NUM_CTX);
        assert_eq!(choose_num_ctx(Some(65_536), None, None, Some(131_072)), 65_536, "unloaded since: the size it had");
        assert_eq!(choose_num_ctx(Some(32_768), Some(131_072), None, Some(131_072)), 32_768, "once chosen, a reading of /api/ps does not move it");
        assert_eq!(choose_num_ctx(Some(32_768), None, None, Some(8192)), 8192, "never above the model");
    }

    #[test]
    fn lines_cut_anywhere_decode_the_same() {
        let body = concat!(
            r#"{"model":"qwen3","message":{"role":"assistant","content":"","thinking":"Plan: "},"done":false}"#, "\n",
            r#"{"model":"qwen3","message":{"role":"assistant","content":"","thinking":"read ✓"},"done":false}"#, "\r\n",
            "\n",
            r#"{"model":"qwen3","message":{"role":"assistant","content":"Héllo"},"done":false}"#, "\n",
            r#"{"model":"qwen3","message":{"role":"assistant","content":""},"done":true,"done_reason":"length","prompt_eval_count":1200,"prompt_eval_cached_count":1100,"eval_count":7}"#, "\n",
        )
        .as_bytes();
        let whole = ok(decode(&[body]));
        assert_eq!(
            whole,
            vec![
                LlmEvent::ReasoningDelta("Plan: ".into()),
                LlmEvent::ReasoningDelta("read ✓".into()),
                LlmEvent::TextDelta("Héllo".into()),
                LlmEvent::Usage(Usage { prompt: Some(1200), completion: Some(7), cached: Some(1100), mtp: None }),
                LlmEvent::Done(FinishReason::Length),
            ]
        );
        for cut in 1..body.len() {
            assert_eq!(ok(decode(&[&body[..cut], &body[cut..]])), whole, "cut at {cut}");
        }
        let bytewise: Vec<&[u8]> = body.chunks(1).collect();
        assert_eq!(ok(decode(&bytewise)), whole);
    }

    #[test]
    fn tool_calls_arrive_whole_and_each_gets_an_index_and_an_id() {
        let body = concat!(
            r#"{"message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":"read_file","arguments":{"path":"a.rs"}}}]},"done":false}"#, "\n",
            r#"{"message":{"role":"assistant","content":"","tool_calls":[{"id":"call_x1","function":{"index":0,"name":"grep","arguments":{"pattern":"fn"}}},{"function":{"name":"list_dir","arguments":{}}}]},"done":false}"#, "\n",
            r#"{"message":{"role":"assistant","content":""},"done":true,"done_reason":"stop"}"#,
        );
        let events = ok(decode(&[body.as_bytes()]));
        let calls: Vec<(usize, String, String, String)> = events
            .iter()
            .filter_map(|e| match e {
                LlmEvent::ToolCallDelta { index, id: Some(id), name: Some(name), args_delta } => Some((*index, id.clone(), name.clone(), args_delta.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(calls.iter().map(|c| c.0).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert_eq!(calls.iter().map(|c| c.2.as_str()).collect::<Vec<_>>(), vec!["read_file", "grep", "list_dir"]);
        assert_eq!(calls[0].3, r#"{"path":"a.rs"}"#);
        assert_eq!(calls[1].1, "call_x1", "the server's id is kept");
        assert!(calls[0].1.len() == 9 && calls[0].1.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(calls[0].1, calls[2].1);
        assert_eq!(events.last(), Some(&LlmEvent::Done(FinishReason::ToolUse)), "the last line said the body ended without a finish line");
    }

    #[test]
    fn an_error_line_ends_the_stream() {
        let body = concat!(
            r#"{"message":{"role":"assistant","content":"par"},"done":false}"#, "\n",
            r#"{"error":"model runner has unexpectedly stopped"}"#, "\n",
            r#"{"message":{"role":"assistant","content":"more"},"done":false}"#, "\n",
        );
        let events = decode(&[body.as_bytes()]);
        assert!(matches!(&events[..], [Ok(LlmEvent::TextDelta(t)), Err(LlmError::Stream(e))] if t == "par" && e.contains("unexpectedly stopped")), "{events:?}");
    }

    #[test]
    fn a_last_line_without_a_newline_is_read_at_the_end() {
        let events = ok(decode(&[br#"{"message":{"role":"assistant","content":"hi"},"done":true,"done_reason":"stop","eval_count":1}"#]));
        assert_eq!(events.first(), Some(&LlmEvent::TextDelta("hi".into())));
        assert_eq!(events.last(), Some(&LlmEvent::Done(FinishReason::Stop)));
    }

    fn show_body(req: &test_server::Request) -> String {
        let name = req.json()["model"].as_str().unwrap_or_default().to_string();
        match name.as_str() {
            "qwen3:8b" => json!({
                "capabilities": ["completion", "tools", "thinking"],
                "details": { "family": "qwen3" },
                "model_info": { "general.architecture": "qwen3", "qwen3.context_length": 40960 },
                "parameters": "temperature 0.6\ntop_k 20",
            }),
            "gpt-oss:20b" => json!({
                "capabilities": ["completion", "tools", "thinking"],
                "details": { "family": "gptoss" },
                "model_info": { "general.architecture": "gptoss", "gptoss.context_length": 131072 },
            }),
            "gemma3:4b" => json!({
                "capabilities": ["completion", "vision"],
                "details": { "family": "gemma3" },
                "model_info": { "general.architecture": "gemma3", "gemma3.context_length": 131072 },
                "parameters": "num_ctx                        24576\nstop \"<end_of_turn>\"",
            }),
            "nomic-embed-text:latest" => json!({
                "capabilities": ["embedding"],
                "model_info": { "general.architecture": "nomic-bert", "nomic-bert.context_length": 2048 },
            }),
            // An Ollama from before `capabilities`: the template says tools.
            "mistral:7b" => json!({
                "template": "{{- if .Tools }}[AVAILABLE_TOOLS] {{ .Tools }}[/AVAILABLE_TOOLS]{{ end }}",
                "details": { "family": "llama" },
                "model_info": { "general.architecture": "llama", "llama.context_length": 32768 },
            }),
            // The newest servers list the `think` values themselves.
            "deepseek-v4:flash" => json!({
                "capabilities": ["completion", "tools", "thinking"],
                "thinking": { "values": [false, true, "high"], "default": true },
                "model_info": { "general.architecture": "deepseek4", "deepseek4.context_length": 1048576 },
            }),
            _ => return r#"{"error":"model not found"}"#.to_string(),
        }
        .to_string()
    }

    /// `loaded` is what `/api/ps` answers, and can change between requests.
    async fn ollama_server(loaded: std::sync::Arc<Mutex<Value>>) -> (String, test_server::Log) {
        test_server::serve(move |req| match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/api/tags") => ("200 OK", "application/json", json!({ "models": [
                { "name": "qwen3:8b", "model": "qwen3:8b", "digest": "d-qwen" },
                { "name": "gpt-oss:20b", "model": "gpt-oss:20b", "digest": "d-oss" },
                { "name": "gemma3:4b", "model": "gemma3:4b", "digest": "d-gemma" },
                { "name": "nomic-embed-text:latest", "model": "nomic-embed-text:latest", "digest": "d-nomic" },
                { "name": "mistral:7b", "model": "mistral:7b", "digest": "d-mistral" },
                { "name": "deepseek-v4:flash", "model": "deepseek-v4:flash", "digest": "d-ds" },
            ] }).to_string()),
            ("GET", "/api/ps") => ("200 OK", "application/json", loaded.lock().to_string()),
            ("POST", "/api/show") => match show_body(req) {
                b if b.contains("\"error\"") => ("404 Not Found", "application/json", b),
                b => ("200 OK", "application/json", b),
            },
            _ => ("404 Not Found", "text/plain", "404 page not found".into()),
        })
        .await
    }

    #[tokio::test]
    async fn discovery_reads_the_installed_models_what_is_loaded_and_what_each_can_do() {
        let loaded = json!({ "models": [ { "name": "gpt-oss:20b", "model": "gpt-oss:20b", "context_length": 65536 } ] });
        let (url, log) = ollama_server(std::sync::Arc::new(Mutex::new(loaded))).await;
        let llm = client(&format!("{url}/api/"), "");
        let disc = llm.discover_server().await.expect("discovered");
        assert_eq!(disc.kind, ServerKind::Ollama);
        let ids: Vec<&str> = disc.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["qwen3:8b", "gpt-oss:20b", "gemma3:4b", "mistral:7b", "deepseek-v4:flash"], "the embedding model is not offered");
        let get = |id: &str| disc.models.iter().find(|m| m.id == id).unwrap().clone();

        let qwen = get("qwen3:8b");
        assert!(qwen.supports_tools && !qwen.supports_vision && !qwen.is_loaded);
        assert_eq!((qwen.context_length, qwen.max_context_length), (Some(DEFAULT_NUM_CTX), Some(40960)));
        assert_eq!(qwen.thinking, profile(&["off", "on"], "on"));

        let oss = get("gpt-oss:20b");
        assert!(oss.is_loaded);
        assert_eq!(oss.context_length, Some(65536), "loaded with enough: kept, no reload");
        assert_eq!(oss.thinking, profile(&["low", "medium", "high"], "medium"));

        let gemma = get("gemma3:4b");
        assert!(gemma.supports_vision && !gemma.supports_tools);
        assert_eq!(gemma.context_length, Some(24576), "the Modelfile's num_ctx");
        assert_eq!(gemma.thinking, no_thinking());

        let mistral = get("mistral:7b");
        assert!(mistral.supports_tools && mistral.thinking.is_unreported());
        assert_eq!(mistral.context_length, Some(DEFAULT_NUM_CTX));

        let deepseek = get("deepseek-v4:flash");
        assert_eq!(deepseek.thinking, profile(&["off", "on", "high"], "on"));

        assert_eq!(llm.model(), "gpt-oss:20b", "the loaded model is the active one");
        assert_eq!(llm.profile(), Some(profile(&["low", "medium", "high"], "medium")));

        // Looked at again: `/api/show` is not asked twice for the same digest.
        let shows = |log: &test_server::Log| log.lock().unwrap().iter().filter(|r| r.path == "/api/show").count();
        let first = shows(&log);
        assert_eq!(first, 6);
        llm.discover_server().await.expect("discovered again");
        assert_eq!(shows(&log), first);
    }

    #[tokio::test]
    async fn the_model_the_user_chose_stays_chosen_while_another_is_loaded() {
        let loaded = json!({ "models": [ { "name": "gpt-oss:20b", "context_length": 65536 } ] });
        let (url, _) = ollama_server(std::sync::Arc::new(Mutex::new(loaded))).await;
        let llm = client(&url, "qwen3:8b");
        let disc = llm.discover_server().await.unwrap();
        assert_eq!(llm.model(), "qwen3:8b", "Ollama loads it when asked");
        assert_eq!(disc.active_model.map(|m| m.id).as_deref(), Some("qwen3:8b"));
        assert_eq!(llm.discovery().and_then(|d| d.active_model).map(|m| m.id).as_deref(), Some("qwen3:8b"));
        assert_eq!(llm.profile(), Some(profile(&["off", "on"], "on")), "the chosen model's thinking, not the loaded one's");
        // A model that is not installed is not kept.
        let llm = client(&url, "llama9:1t");
        llm.discover_server().await.unwrap();
        assert_eq!(llm.model(), "gpt-oss:20b");
    }

    #[tokio::test]
    async fn a_context_chosen_once_stays_when_the_model_is_unloaded() {
        // Loaded by someone else with 64k: adopted. Unloaded later (keep_alive ran out):
        // the next request must not load it with another size.
        let loaded = std::sync::Arc::new(Mutex::new(json!({ "models": [ { "name": "gpt-oss:20b", "context_length": 65536 } ] })));
        let (url, _) = ollama_server(loaded.clone()).await;
        let llm = client(&url, "gpt-oss:20b");
        let num_ctx = |llm: &Client| body(llm, &[ChatMessage::user("hi")], &[], &TurnOptions::default())["options"]["num_ctx"].clone();
        llm.discover_server().await.unwrap();
        assert_eq!(num_ctx(&llm), 65536);
        *loaded.lock() = json!({ "models": [] });
        let disc = llm.discover_server().await.unwrap();
        assert!(!disc.models.iter().any(|m| m.is_loaded));
        assert_eq!(num_ctx(&llm), 65536);
        // Another server knows nothing of it.
        llm.set_endpoint(Endpoint::new(ApiProtocol::Ollama, url, Some("k".into())));
        llm.discover_server().await.unwrap();
        assert_eq!(num_ctx(&llm), DEFAULT_NUM_CTX);
    }

    #[tokio::test]
    async fn a_server_without_ps_or_show_still_lists_its_models() {
        let (url, _) = test_server::serve(|req| match req.path.as_str() {
            "/api/tags" => ("200 OK", "application/json", r#"{"models":[{"name":"llama3.2:latest"}]}"#.into()),
            _ => ("404 Not Found", "text/plain", "404 page not found".into()),
        })
        .await;
        let llm = client(&url, "");
        let disc = llm.discover_server().await.expect("the list alone is enough");
        let m = &disc.models[0];
        assert_eq!((m.id.as_str(), m.context_length, m.is_loaded), ("llama3.2:latest", Some(DEFAULT_NUM_CTX), false));
        assert!(m.supports_tools && m.thinking.is_unreported());
        assert!(client("http://127.0.0.1:9", "").discover_server().await.is_none(), "nothing there");
        // The app looks at the server from a spawned task.
        let shared = std::sync::Arc::new(client(&url, ""));
        let found = tokio::spawn(async move { shared.discover_server().await }).await.unwrap();
        assert!(found.is_some());
    }

    #[tokio::test]
    async fn a_turn_streams_from_api_chat_through_the_client() {
        let (url, log) = test_server::serve(|req| match req.path.as_str() {
            "/api/chat" => ("200 OK", "application/x-ndjson", concat!(
                r#"{"model":"qwen3:8b","message":{"role":"assistant","content":"","thinking":"hmm"},"done":false}"#, "\n",
                r#"{"model":"qwen3:8b","message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":"read_file","arguments":{"path":"a.rs"}}}]},"done":false}"#, "\n",
                r#"{"model":"qwen3:8b","message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","prompt_eval_count":900,"eval_count":30}"#, "\n",
            ).to_string()),
            _ => ("404 Not Found", "text/plain", "404 page not found".into()),
        })
        .await;
        let llm = Client::new(Endpoint::new(ApiProtocol::Ollama, url, None), "qwen3:8b").with_profile(profile(&["off", "on"], "on"));
        let options = TurnOptions { thinking: ThinkingEffort::Off, max_tokens: Some(64), ..Default::default() };
        let tools = [ToolSpec { name: "read_file".into(), description: "read".into(), parameters_json: r#"{"type":"object"}"#.into() }];
        let events: Vec<LlmEvent> = llm.stream_with_options(&[ChatMessage::user("read a.rs")], &tools, &options).await.unwrap().map(|e| e.unwrap()).collect().await;
        assert_eq!(events[0], LlmEvent::ReasoningDelta("hmm".into()));
        assert!(matches!(&events[1], LlmEvent::ToolCallDelta { index: 0, name: Some(n), args_delta, .. } if n == "read_file" && args_delta == r#"{"path":"a.rs"}"#));
        assert_eq!(events[2], LlmEvent::Usage(Usage { prompt: Some(900), completion: Some(30), cached: None, mtp: None }));
        assert_eq!(events[3], LlmEvent::Done(FinishReason::ToolUse));
        assert_eq!(llm.requests_in_flight(), 0);
        let sent = log.lock().unwrap()[0].clone();
        assert_eq!((sent.method.as_str(), sent.path.as_str()), ("POST", "/api/chat"));
        let sent = sent.json();
        assert_eq!(sent["think"], false);
        assert_eq!(sent["options"]["num_predict"], 64);
        assert_eq!(sent["options"]["num_ctx"], DEFAULT_NUM_CTX);
        assert_eq!(sent["tools"][0]["function"]["name"], "read_file");
    }

    #[tokio::test]
    async fn a_model_that_refuses_thinking_is_asked_again_without_it() {
        let (url, log) = test_server::serve(|req| {
            if req.json().get("think").is_some() {
                ("400 Bad Request", "application/json", r#"{"error":"\"llama3.2\" does not support thinking"}"#.into())
            } else {
                ("200 OK", "application/x-ndjson", r#"{"message":{"role":"assistant","content":"hi"},"done":true,"done_reason":"stop"}"#.into())
            }
        })
        .await;
        let llm = client(&url, "llama3.2").with_profile(profile(&["off", "on"], "on"));
        let options = TurnOptions { thinking: ThinkingEffort::High, ..Default::default() };
        for _ in 0..2 {
            let events: Vec<LlmEvent> = llm.stream_with_options(&[ChatMessage::user("hi")], &[], &options).await.unwrap().map(|e| e.unwrap()).collect().await;
            assert_eq!(events[0], LlmEvent::TextDelta("hi".into()));
        }
        assert_eq!(log.lock().unwrap().len(), 3, "refused once, then known");
        assert_eq!(llm.profile(), Some(no_thinking()));

        let (url, _) = test_server::serve(|_| ("404 Not Found", "application/json", r#"{"error":"model \"nope\" not found, try pulling it first"}"#.into())).await;
        let err = client(&url, "nope").stream(&[ChatMessage::user("hi")], &[]).await.err().expect("an error");
        assert!(matches!(err, LlmError::Status { status: 404, ref body } if body.contains("try pulling it")), "{err}");
    }

    #[test]
    fn the_address_may_name_the_api_path() {
        for url in ["http://localhost:11434", "http://localhost:11434/", "http://localhost:11434/api", "http://localhost:11434/api/", "http://localhost:11434/v1"] {
            assert_eq!(root(&client(url, "m")), "http://localhost:11434", "{url}");
        }
    }
}
