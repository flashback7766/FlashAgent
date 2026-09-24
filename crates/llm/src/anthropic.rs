//! Anthropic's Messages API (`/v1/messages`). Signed thinking is kept and sent
//! back as it came; the tools, the system prompt and the newest message carry
//! cache breakpoints; the thinking a turn asks for becomes adaptive thinking
//! with an effort, or a token budget, whichever the model takes. A request the
//! API refuses for a reason it names (a thinking block it no longer accepts,
//! an output cap, a field an older model lacks) is corrected and sent again.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::LazyLock;
use std::time::Duration;

use parking_lot::Mutex;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde_json::{json, Value};

use crate::client::{pump, Client, EventStream, WireDecoder};
use crate::repair::repair_json;
use crate::thinking::{DiscoveredModel, ServerDiscovery, ServerKind, ThinkingProfile, ThinkingProtocol};
use crate::types::{ChatMessage, FinishReason, LlmError, LlmEvent, Role, ThinkingEffort, ToolCall, ToolSpec, TurnOptions, Usage};

const API_VERSION: &str = "2023-06-01";
/// A stream has no request timeout to stay under, and max_tokens does not
/// count against the output rate limit, so the answer gets room.
const DEFAULT_MAX_TOKENS: u32 = 64_000;
const MAX_ATTEMPTS: usize = 4;
const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Replayed thinking blocks the API refused (bound to a conversation prefix
/// that has since changed), by fingerprint. The history that carries them does
/// not change, so they are left out from then on instead of being refused on
/// every request.
static STALE: LazyLock<Mutex<HashSet<u64>>> = LazyLock::new(Default::default);
/// Output caps a server named when it refused a request, per address and model.
static CAPS: LazyLock<Mutex<HashMap<String, u32>>> = LazyLock::new(Default::default);
/// Gateways that refused `x-api-key` and took the key as a bearer token.
static BEARER: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    None,
    /// `thinking: {type: "enabled", budget_tokens}`.
    Budget,
    /// `thinking: {type: "adaptive"}`, its depth set by `output_config.effort`.
    Adaptive,
}

/// What a model takes, read from its id. An unknown Claude id is taken for the
/// newest kind; any other name (DeepSeek, GLM, Kimi behind an
/// Anthropic-compatible endpoint) for the budget form those servers copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Family {
    mode: Mode,
    /// Accepts `{type: "disabled"}`: Fable and Opus 5.5 always think.
    can_disable: bool,
    /// Thinks when `thinking` is left out, at `default_effort`.
    thinks_by_default: bool,
    default_effort: &'static str,
    efforts: &'static [&'static str],
    /// Takes `thinking.display`. These models leave the thinking text empty unless asked.
    display: bool,
    /// temperature, top_p and top_k, removed from Opus 4.7 and Sonnet 5 on.
    sampling: bool,
    vision: bool,
    /// Tokens; 0 when unknown.
    context: usize,
    max_output: u32,
}

const ADAPTIVE: Family = Family {
    mode: Mode::Adaptive,
    can_disable: true,
    thinks_by_default: true,
    default_effort: "high",
    efforts: &EFFORTS,
    display: true,
    sampling: false,
    vision: true,
    context: 1_000_000,
    max_output: 128_000,
};

const BUDGET: Family = Family {
    mode: Mode::Budget,
    can_disable: true,
    thinks_by_default: false,
    default_effort: "high",
    efforts: &[],
    display: false,
    sampling: true,
    vision: true,
    context: 200_000,
    max_output: 64_000,
};

/// Handles `claude-opus-4-8`, `claude-3-7-sonnet-20250219`,
/// `anthropic/claude-sonnet-4.5` and `us.anthropic.claude-opus-4-1-20250805-v1:0`.
fn family(model: &str) -> Family {
    let id = model.to_ascii_lowercase();
    let Some(rest) = id.split("claude-").nth(1) else {
        return Family { vision: false, context: 0, max_output: 8_192, ..BUDGET };
    };
    let words: Vec<&str> = rest.split(|c: char| !c.is_ascii_alphanumeric()).collect();
    let tier = words.iter().copied().find(|w| matches!(*w, "opus" | "sonnet" | "haiku" | "fable" | "mythos")).unwrap_or_default();
    // Dates and `v1` are not version numbers.
    let mut numbers = words.iter().filter(|w| (1..=2).contains(&w.len()) && w.bytes().all(|b| b.is_ascii_digit())).filter_map(|w| w.parse::<u32>().ok());
    let version = numbers.next().map(|major| major * 10 + numbers.next().unwrap_or(0).min(9)).unwrap_or(u32::MAX);
    let always = Family { can_disable: false, ..ADAPTIVE };
    match tier {
        "fable" | "mythos" => always,
        "opus" if version >= 55 => Family { default_effort: "medium", ..always },
        "opus" if version >= 50 => ADAPTIVE,
        "opus" if version >= 47 => Family { thinks_by_default: false, ..ADAPTIVE },
        "sonnet" | "haiku" if version >= 50 => ADAPTIVE,
        "opus" | "sonnet" if version >= 46 => Family {
            thinks_by_default: false,
            efforts: &["low", "medium", "high", "max"],
            display: false,
            sampling: true,
            ..ADAPTIVE
        },
        "opus" if version >= 45 => BUDGET,
        "opus" if version >= 40 => Family { max_output: 32_000, ..BUDGET },
        "sonnet" if version >= 37 => BUDGET,
        "haiku" if version >= 45 => BUDGET,
        "opus" | "sonnet" | "haiku" => Family { mode: Mode::None, max_output: if version >= 35 { 8_192 } else { 4_096 }, ..BUDGET },
        _ => ADAPTIVE,
    }
}

fn profile(f: &Family) -> ThinkingProfile {
    let presets: Vec<&str> = match f.mode {
        Mode::None => return ThinkingProfile::unsupported(),
        Mode::Budget => vec!["off", "low", "medium", "high"],
        Mode::Adaptive => f.can_disable.then_some("off").into_iter().chain(f.efforts.iter().copied()).collect(),
    };
    let default = if f.mode == Mode::Adaptive && f.thinks_by_default { f.default_effort } else { "off" };
    ThinkingProfile {
        presets: presets.into_iter().map(String::from).collect(),
        protocol: ThinkingProtocol::Anthropic,
        supported: true,
        default_preset: Some(default.to_string()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Think {
    /// Nothing sent: the model's own default.
    Omit,
    Disabled,
    Adaptive(Option<&'static str>),
    Budget(u32),
}

impl Think {
    fn on(self) -> bool {
        matches!(self, Think::Adaptive(_) | Think::Budget(_))
    }
}

/// A preset name (`off`, `low` … `max`, or one typed by hand) as this model takes it.
fn think_for(f: &Family, effort: Option<&str>) -> Think {
    let Some(effort) = effort.map(|e| e.trim().to_ascii_lowercase()) else {
        // Asked for explicitly only so the summary is shown: the same thinking the model does anyway.
        return if f.mode == Mode::Adaptive && f.thinks_by_default && f.display { Think::Adaptive(None) } else { Think::Omit };
    };
    let off = matches!(effort.as_str(), "off" | "none" | "disabled" | "false" | "0" | "no");
    let level = match effort.as_str() {
        "minimal" | "min" | "low" => Some(0),
        "medium" => Some(1),
        "high" => Some(2),
        "xhigh" | "extra-high" => Some(3),
        "max" => Some(4),
        _ => None,
    };
    match f.mode {
        Mode::None => Think::Omit,
        Mode::Budget if off => Think::Omit,
        Mode::Budget => Think::Budget([4_096, 12_288, 24_576, 32_000, 32_000][level.unwrap_or(1)]),
        Mode::Adaptive if off && f.can_disable => Think::Disabled,
        Mode::Adaptive if off => Think::Adaptive(f.efforts.first().copied()),
        // A level the model lacks becomes the nearest one below it.
        Mode::Adaptive => Think::Adaptive(level.map(|l| EFFORTS[..=l].iter().rev().find(|e| f.efforts.contains(e)).or(f.efforts.first()).copied().unwrap_or("high"))),
    }
}

/// How one request is shaped. A refusal that names what was wrong changes it.
#[derive(Debug, Clone)]
struct Plan {
    family: Family,
    think: Think,
    max_tokens: u32,
    sampling: bool,
    display: bool,
    /// Tool input streamed as it is written. Only the real API is sure to take
    /// the field; gateways that copy it may not.
    eager: bool,
}

fn plan(client: &Client, model: &str, messages: &[ChatMessage], options: &TurnOptions) -> Plan {
    let family = family(model);
    let base = client.base_url();
    // Adaptive thinking already sizes itself to each request, and an effort
    // that changed with each prompt would re-bill the whole conversation uncached.
    let think = if family.mode == Mode::Adaptive && options.thinking == ThinkingEffort::Auto && options.custom_effort.is_none() {
        Think::Adaptive(None)
    } else {
        think_for(&family, client.resolve_effort(messages, options).as_deref())
    };
    let cap = CAPS.lock().get(&cap_key(&base, model)).copied().unwrap_or(u32::MAX);
    let max_tokens = options.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS.min(family.max_output)).min(cap).max(1);
    Plan { family, think, max_tokens, sampling: family.sampling, display: family.display, eager: is_official(&base) }
}

impl Plan {
    /// Changes what the refusal named; false when nothing here can help.
    fn recover(&mut self, message: &str, sent: &Value, cap_key: &str) -> bool {
        let lower = message.to_ascii_lowercase();
        if lower.contains("thinking") && (lower.contains("signature") || lower.contains("cannot be modified")) {
            return mark_stale(message, sent);
        }
        if lower.contains("must start with a thinking block") || lower.contains("expected `thinking`") {
            return self.think_off();
        }
        if lower.contains("context limit") {
            return match context_room(&lower) {
                Some(room) if room < self.max_tokens => {
                    self.max_tokens = room;
                    true
                }
                _ => false,
            };
        }
        if let Some(cap) = named_cap(&lower, self.max_tokens) {
            CAPS.lock().insert(cap_key.to_string(), cap);
            self.max_tokens = cap;
            return true;
        }
        if self.eager && lower.contains("eager_input_streaming") {
            self.eager = false;
            return true;
        }
        if self.sampling && ["temperature", "top_p", "top_k"].iter().any(|f| lower.contains(f)) {
            self.sampling = false;
            return true;
        }
        if self.display && lower.contains("display") {
            self.display = false;
            return true;
        }
        if matches!(self.think, Think::Adaptive(Some(_))) && (lower.contains("effort") || lower.contains("output_config")) {
            self.think = Think::Adaptive(None);
            return true;
        }
        if self.think != Think::Omit && lower.contains("thinking") {
            self.think = Think::Omit;
            return true;
        }
        false
    }

    fn think_off(&mut self) -> bool {
        let off = if !self.family.thinks_by_default {
            Think::Omit
        } else if self.family.can_disable {
            Think::Disabled
        } else {
            return false;
        };
        let changed = self.think != off;
        self.think = off;
        changed
    }
}

fn host(url: &str) -> &str {
    url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or_default()
}

fn is_official(base: &str) -> bool {
    host(root(base)).eq_ignore_ascii_case("api.anthropic.com")
}

/// `https://api.anthropic.com`, `…/v1` and `…/v1/messages` all name the same
/// API, and no address at all means Anthropic's own.
fn root(base: &str) -> &str {
    if base.is_empty() {
        return "https://api.anthropic.com";
    }
    base.strip_suffix("/v1/messages").or_else(|| base.strip_suffix("/v1")).unwrap_or(base)
}

fn cap_key(base: &str, model: &str) -> String {
    format!("{base}\n{model}")
}

fn auth(key: Option<&str>, bearer: bool) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("anthropic-version", HeaderValue::from_static(API_VERSION));
    if let Some(key) = key {
        if bearer {
            if let Ok(value) = HeaderValue::from_str(&format!("Bearer {key}")) {
                headers.insert(AUTHORIZATION, value);
            }
        } else if let Ok(value) = HeaderValue::from_str(key) {
            headers.insert("x-api-key", value);
        }
    }
    headers
}

fn headers(client: &Client) -> HeaderMap {
    auth(client.api_key().as_deref(), BEARER.lock().contains(&client.base_url()))
}

pub(crate) async fn stream(
    client: &Client,
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    options: &TurnOptions,
) -> Result<EventStream, LlmError> {
    let busy = client.busy();
    let model = client.model();
    let base = client.base_url();
    // Without a model list (a gateway that has none) the presets still follow the model.
    if client.profile().is_none() {
        client.set_profile(profile(&family(&model)));
    }
    let mut plan = plan(client, &model, messages, options);
    let url = format!("{}/v1/messages", root(&base));
    let key = cap_key(&base, &model);
    let mut may_try_bearer = !is_official(&base) && client.api_key().is_some() && !BEARER.lock().contains(&base);
    let mut refused_key: Option<LlmError> = None;
    let mut attempts = 0;
    loop {
        attempts += 1;
        let body = body(&model, messages, tools, options, &plan);
        let resp = client.post(&url, &headers(client), &body).await?;
        if resp.status().is_success() {
            return Ok(pump(resp, busy, Decoder::default()));
        }
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if matches!(status, 401 | 403) {
            // Some gateways take the key only as a bearer token.
            if may_try_bearer {
                may_try_bearer = false;
                BEARER.lock().insert(base.clone());
                refused_key = Some(LlmError::Status { status, body: text });
                continue;
            }
            if let Some(first) = refused_key {
                BEARER.lock().remove(&base);
                return Err(first);
            }
        }
        if status != 400 || attempts >= MAX_ATTEMPTS || !plan.recover(&error_message(&text), &body, &key) {
            return Err(LlmError::Status { status, body: text });
        }
    }
}

fn error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.to_string())
}

fn numbers(text: &str) -> Vec<u64> {
    text.split(|c: char| !c.is_ascii_digit()).filter_map(|w| w.parse().ok()).collect()
}

/// "input length and `max_tokens` exceed context limit: 188240 + 21333 > 200000"
fn context_room(message: &str) -> Option<u32> {
    let tail = message.split("context limit").nth(1)?;
    let [input, _, limit, ..] = numbers(tail)[..] else { return None };
    limit.checked_sub(input).filter(|&room| room > 0).map(|room| room.min(u32::MAX as u64) as u32)
}

/// "max_tokens: 64000 > 32000, which is the maximum allowed number of output
/// tokens for …": the largest plausible number below what was sent. A
/// complaint about the thinking budget quotes the budget, not a cap.
fn named_cap(message: &str, sent: u32) -> Option<u32> {
    if !message.contains("max_tokens") || message.contains("budget") {
        return None;
    }
    numbers(message).into_iter().filter(|&n| (1024..sent as u64).contains(&n)).max().map(|n| n as u32)
}

fn is_thinking(block: &Value) -> bool {
    matches!(block["type"].as_str(), Some("thinking" | "redacted_thinking"))
}

fn fingerprint(block: &Value) -> Option<u64> {
    let key = block["signature"].as_str().or(block["data"].as_str())?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    Some(hasher.finish())
}

/// `messages.5.content.0: Invalid signature …` → (5, 0).
fn block_path(message: &str) -> Option<(usize, usize)> {
    let after = message.split("messages.").nth(1)?;
    let index = |s: &str| s.chars().take_while(char::is_ascii_digit).collect::<String>().parse::<usize>().ok();
    let message_index = index(after)?;
    Some((message_index, after.split("content.").nth(1).and_then(index).unwrap_or(0)))
}

/// The message holding the block the API named, and every one after it: each
/// block is bound to what came before it, so a later one cannot outlive an
/// earlier one, and the thinking of one message is kept or dropped whole.
fn mark_stale(message: &str, sent: &Value) -> bool {
    let replayed: Vec<((usize, usize), u64)> = sent["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .flat_map(|(m, msg)| {
            msg["content"].as_array().into_iter().flatten().enumerate().filter_map(move |(b, block)| {
                is_thinking(block).then(|| fingerprint(block).map(|fp| ((m, b), fp))).flatten()
            })
        })
        .collect();
    if replayed.is_empty() {
        return false;
    }
    let from = block_path(message).and_then(|(at, _)| replayed.iter().position(|((m, _), _)| *m >= at)).unwrap_or(0);
    STALE.lock().extend(replayed[from..].iter().map(|(_, fp)| *fp));
    true
}

fn push_text(blocks: &mut Vec<Value>, text: &str) {
    // The API refuses a text block with nothing but whitespace.
    if !text.trim().is_empty() {
        blocks.push(json!({ "type": "text", "text": text }));
    }
}

fn image(url: &str) -> Option<Value> {
    let Some(rest) = url.strip_prefix("data:") else {
        return (url.starts_with("https://") || url.starts_with("http://")).then(|| json!({ "type": "image", "source": { "type": "url", "url": url } }));
    };
    let (meta, data) = rest.split_once(',')?;
    let media = match meta.strip_suffix(";base64")?.to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => "image/jpeg",
        "image/png" => "image/png",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        _ => return None,
    };
    Some(json!({ "type": "image", "source": { "type": "base64", "media_type": media, "data": data } }))
}

/// Ids written by another provider may hold characters Anthropic refuses;
/// the same id maps to the same text, so a call and its result still match.
fn tool_id(id: &str) -> String {
    let clean: String = id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    if clean.is_empty() { "call".to_string() } else { clean }
}

fn tool_use(call: &ToolCall) -> Value {
    let input = repair_json(&call.args_json)
        .and_then(|fixed| serde_json::from_str::<Value>(&fixed).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    json!({ "type": "tool_use", "id": tool_id(&call.id), "name": call.name, "input": input })
}

fn tool_result(m: &ChatMessage) -> Value {
    let mut block = json!({ "type": "tool_result", "tool_use_id": tool_id(m.tool_call_id.as_deref().unwrap_or_default()) });
    if !m.content.trim().is_empty() {
        block["content"] = json!(m.content);
    }
    block
}

fn user_blocks(m: &ChatMessage) -> Vec<Value> {
    let mut blocks = Vec::new();
    push_text(&mut blocks, &m.content);
    blocks.extend(m.images.iter().filter_map(|url| image(url)));
    blocks
}

/// The replay records where each thinking block stood among the text and the
/// tool calls. While the message is still what was streamed, the blocks go
/// back in that order; otherwise (a continuation merged in, text the loop
/// cleaned) thinking first, then text, then the calls.
fn assistant_blocks(m: &ChatMessage, thinks: bool, stale: &HashSet<u64>) -> Vec<Value> {
    let layout: &[Value] = m
        .replay
        .as_ref()
        .filter(|r| r["protocol"] == "anthropic" && thinks)
        .and_then(|r| r["blocks"].as_array())
        .map(Vec::as_slice)
        .unwrap_or_default();
    let keep = |b: &Value| is_thinking(b) && fingerprint(b).is_some_and(|fp| !stale.contains(&fp));
    let lens: Vec<usize> = layout.iter().filter(|b| b["type"] == "text").filter_map(|b| b["len"].as_u64()).map(|n| n as usize).collect();
    let ids: Vec<&str> = layout.iter().filter(|b| b["type"] == "tool_use").filter_map(|b| b["id"].as_str()).collect();
    let mut at = 0;
    let exact = lens.iter().all(|n| {
        at += n;
        m.content.is_char_boundary(at)
    }) && at == m.content.len()
        && ids.len() == m.tool_calls.len()
        && ids.iter().zip(&m.tool_calls).all(|(id, call)| *id == call.id);

    let mut blocks = Vec::new();
    if exact {
        let (mut at, mut calls) = (0, m.tool_calls.iter());
        for b in layout {
            match b["type"].as_str() {
                Some("text") => {
                    let n = b["len"].as_u64().unwrap_or(0) as usize;
                    push_text(&mut blocks, &m.content[at..at + n]);
                    at += n;
                }
                Some("tool_use") => blocks.extend(calls.next().map(tool_use)),
                _ if keep(b) => blocks.push(b.clone()),
                _ => {}
            }
        }
    } else {
        blocks.extend(layout.iter().filter(|b| keep(b)).cloned());
        push_text(&mut blocks, &m.content);
        blocks.extend(m.tool_calls.iter().map(tool_use));
    }
    // An answer cut off while it was still thinking has nothing to answer with.
    if blocks.iter().all(is_thinking) {
        blocks.clear();
    }
    blocks
}

type Turn = (Role, Vec<Value>);

/// Messages must alternate: one of the same role joins the one before it.
fn push(turns: &mut Vec<Turn>, role: Role, blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    match turns.last_mut() {
        Some((last, existing)) if *last == role => existing.extend(blocks),
        _ => turns.push((role, blocks)),
    }
}

fn ids_of<'a>(blocks: &'a [Value], kind: &str, field: &str) -> Vec<&'a str> {
    blocks.iter().filter(|b| b["type"] == kind).filter_map(|b| b[field].as_str()).collect()
}

/// Every tool_use needs its tool_result at the start of the very next message,
/// and every result a call just before it. A history cut by compaction or
/// carried over from another provider may lack either, and the API refuses both.
fn pair_tool_calls(turns: &mut Vec<Turn>) {
    let mut i = 0;
    while i < turns.len() {
        if turns[i].0 == Role::Assistant {
            let calls: Vec<String> = ids_of(&turns[i].1, "tool_use", "id").into_iter().map(String::from).collect();
            if !calls.is_empty() {
                if !matches!(turns.get(i + 1), Some((Role::User, _))) {
                    turns.insert(i + 1, (Role::User, Vec::new()));
                }
                let answered: HashSet<String> = ids_of(&turns[i + 1].1, "tool_result", "tool_use_id").into_iter().map(String::from).collect();
                let missing: Vec<Value> = calls
                    .iter()
                    .filter(|id| !answered.contains(*id))
                    .map(|id| json!({ "type": "tool_result", "tool_use_id": id, "content": "No result was recorded for this call.", "is_error": true }))
                    .collect();
                turns[i + 1].1.splice(0..0, missing);
            }
        } else {
            let calls: HashSet<String> = match i.checked_sub(1).map(|p| &turns[p]) {
                Some((Role::Assistant, blocks)) => ids_of(blocks, "tool_use", "id").into_iter().map(String::from).collect(),
                _ => HashSet::new(),
            };
            for block in turns[i].1.iter_mut() {
                let Some(id) = block["tool_use_id"].as_str().filter(|_| block["type"] == "tool_result") else { continue };
                if !calls.contains(id) {
                    let output = block["content"].as_str().unwrap_or("(no output)");
                    *block = json!({ "type": "text", "text": format!("Result of tool call {id}:\n{output}") });
                }
            }
            turns[i].1.sort_by_key(|b| b["type"] != "tool_result");
        }
        i += 1;
    }
}

/// Budget thinking cannot change inside a tool loop: the last assistant
/// message must start with thinking while it is on and hold none while it is
/// off. A change waits for the next prompt.
fn one_mode_per_turn(turns: &mut [Turn], think: Think) -> Think {
    let [.., (Role::Assistant, assistant), (Role::User, user)] = turns else { return think };
    if !user.iter().any(|b| b["type"] == "tool_result") {
        return think;
    }
    let starts_with_thinking = assistant.first().is_some_and(is_thinking);
    match think {
        Think::Budget(_) if !starts_with_thinking => Think::Omit,
        Think::Budget(_) => think,
        _ => {
            assistant.retain(|b| !is_thinking(b));
            think
        }
    }
}

fn body(model: &str, messages: &[ChatMessage], tools: &[ToolSpec], options: &TurnOptions, plan: &Plan) -> Value {
    let stale = STALE.lock().clone();
    let mut system = Vec::new();
    let mut turns: Vec<Turn> = Vec::new();
    for m in messages {
        match m.role {
            // Byte for byte as written: the system prompt is the cache prefix.
            Role::System => push_text(&mut system, &m.content),
            Role::User => push(&mut turns, Role::User, user_blocks(m)),
            Role::Tool => push(&mut turns, Role::User, vec![tool_result(m)]),
            Role::Assistant => push(&mut turns, Role::Assistant, assistant_blocks(m, plan.family.mode != Mode::None, &stale)),
        }
    }
    pair_tool_calls(&mut turns);

    let mut think = match plan.think {
        // The budget must stay under max_tokens and leave the answer some room.
        Think::Budget(_) if plan.max_tokens < 2_048 => Think::Omit,
        Think::Budget(budget) => Think::Budget(budget.min(plan.max_tokens - 1_024)),
        other => other,
    };
    if plan.family.mode == Mode::Budget {
        think = one_mode_per_turn(&mut turns, think);
    }

    // Three breakpoints of the four allowed: tools, system, and the newest
    // block, which the next request reads back as its prefix.
    let breakpoint = json!({ "type": "ephemeral" });
    let mut messages: Vec<Value> = turns.into_iter().map(|(role, blocks)| json!({ "role": role.as_str(), "content": blocks })).collect();
    if let Some(block) = messages
        .last_mut()
        .and_then(|m| m["content"].as_array_mut())
        .and_then(|blocks| blocks.iter_mut().rev().find(|b| !is_thinking(b)))
    {
        block["cache_control"] = breakpoint.clone();
    }
    if let Some(last) = system.last_mut() {
        last["cache_control"] = breakpoint.clone();
    }

    let mut body = json!({
        "model": model,
        "max_tokens": plan.max_tokens,
        "stream": true,
        "messages": messages,
    });
    if !system.is_empty() {
        body["system"] = Value::Array(system);
    }
    if !tools.is_empty() {
        let mut specs: Vec<Value> = tools
            .iter()
            .map(|t| {
                let mut schema = serde_json::from_str::<Value>(&t.parameters_json).ok().filter(Value::is_object).unwrap_or_else(|| json!({}));
                if schema.get("type").is_none() {
                    schema["type"] = json!("object");
                }
                let mut spec = json!({ "name": t.name, "description": t.description, "input_schema": schema });
                if plan.eager {
                    spec["eager_input_streaming"] = json!(true);
                }
                spec
            })
            .collect();
        if let Some(last) = specs.last_mut() {
            last["cache_control"] = breakpoint;
        }
        body["tools"] = Value::Array(specs);
    }

    match think {
        Think::Omit => {}
        Think::Disabled => body["thinking"] = json!({ "type": "disabled" }),
        Think::Adaptive(effort) => {
            body["thinking"] = if plan.display { json!({ "type": "adaptive", "display": "summarized" }) } else { json!({ "type": "adaptive" }) };
            if let Some(effort) = effort {
                body["output_config"] = json!({ "effort": effort });
            }
        }
        Think::Budget(budget) => body["thinking"] = json!({ "type": "enabled", "budget_tokens": budget }),
    }
    // Refused alongside thinking, and on the newest models at all. Some models
    // take temperature or top_p but not both; repeat, presence and min_p are
    // not in this API.
    if plan.sampling && !think.on() {
        if let Some(t) = options.temperature {
            body["temperature"] = json!(t.clamp(0.0, 1.0));
        } else if let Some(p) = options.top_p {
            body["top_p"] = json!(p);
        }
        if let Some(k) = options.top_k {
            body["top_k"] = json!(k);
        }
    }
    body
}

/// `event:` and `data:` records. Bytes wait until their line ends, so a chunk
/// may split a line, or a character, anywhere.
#[derive(Default)]
struct Sse {
    buf: Vec<u8>,
    event: String,
    data: String,
}

impl Sse {
    fn feed(&mut self, bytes: &[u8]) -> Vec<(String, String)> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(end) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=end).collect();
            self.line(&String::from_utf8_lossy(&line), &mut out);
        }
        out
    }

    fn finish(&mut self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let rest = std::mem::take(&mut self.buf);
        self.line(&String::from_utf8_lossy(&rest), &mut out);
        self.line("", &mut out);
        out
    }

    fn line(&mut self, line: &str, out: &mut Vec<(String, String)>) {
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if !self.data.is_empty() {
                out.push((std::mem::take(&mut self.event), std::mem::take(&mut self.data)));
            }
            self.event.clear();
            return;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => self.event = value.to_string(),
            "data" => {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(value);
            }
            _ => {}
        }
    }
}

enum Block {
    /// Bytes of text so far.
    Text(usize),
    Thinking { text: String, signature: String },
    Redacted(String),
    Tool { index: usize, id: String, args: bool },
    Other,
}

#[derive(Default)]
struct Tokens {
    seen: bool,
    input: i64,
    written: i64,
    read: i64,
    output: i64,
}

impl Tokens {
    /// `message_start` has the input; `message_delta` the running output count.
    fn read_from(&mut self, usage: &Value) {
        if !usage.is_object() {
            return;
        }
        self.seen = true;
        for (slot, key) in [
            (&mut self.input, "input_tokens"),
            (&mut self.written, "cache_creation_input_tokens"),
            (&mut self.read, "cache_read_input_tokens"),
            (&mut self.output, "output_tokens"),
        ] {
            if let Some(n) = usage[key].as_i64() {
                *slot = n;
            }
        }
    }
}

#[derive(Default)]
struct Decoder {
    sse: Sse,
    /// Content blocks by their index, in the order they started.
    blocks: Vec<(u64, Block)>,
    tool_calls: usize,
    text_seen: bool,
    tokens: Tokens,
    stop: Option<FinishReason>,
    done: bool,
}

impl WireDecoder for Decoder {
    fn feed(&mut self, bytes: &[u8]) -> Vec<Result<LlmEvent, LlmError>> {
        let mut out = Vec::new();
        for (name, data) in self.sse.feed(bytes) {
            self.event(&name, &data, &mut out);
        }
        out
    }

    fn finish(&mut self) -> Vec<Result<LlmEvent, LlmError>> {
        let mut out = Vec::new();
        for (name, data) in self.sse.finish() {
            self.event(&name, &data, &mut out);
        }
        self.end(&mut out);
        out
    }
}

impl Decoder {
    fn event(&mut self, name: &str, data: &str, out: &mut Vec<Result<LlmEvent, LlmError>>) {
        if self.done {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else { return };
        let index = v["index"].as_u64().unwrap_or(0);
        match v["type"].as_str().unwrap_or(name) {
            "message_start" => self.tokens.read_from(&v["message"]["usage"]),
            "content_block_start" => self.start(index, &v["content_block"], out),
            "content_block_delta" => self.delta(index, &v["delta"], out),
            "content_block_stop" => {
                // A call to a tool without parameters may stream no input at all.
                if let Some((_, Block::Tool { index, args, .. })) = self.blocks.iter_mut().rev().find(|(i, _)| *i == index) {
                    if !*args {
                        *args = true;
                        out.push(Ok(LlmEvent::ToolCallDelta { index: *index, id: None, name: None, args_delta: "{}".into() }));
                    }
                }
            }
            "message_delta" => {
                self.tokens.read_from(&v["usage"]);
                if let Some(reason) = v["delta"]["stop_reason"].as_str() {
                    self.stop = Some(self.finish_reason(reason, &v["delta"]["stop_details"], out));
                }
            }
            "message_stop" => self.end(out),
            "error" => {
                let error = &v["error"];
                let message = error["message"].as_str().or(error["type"].as_str()).map(str::to_string).unwrap_or_else(|| v.to_string());
                out.push(Err(LlmError::Stream(message)));
                self.done = true;
            }
            // ping, and whatever a newer API adds.
            _ => {}
        }
    }

    fn start(&mut self, index: u64, block: &Value, out: &mut Vec<Result<LlmEvent, LlmError>>) {
        let text = |key: &str| block[key].as_str().unwrap_or_default().to_string();
        let started = match block["type"].as_str().unwrap_or_default() {
            "text" => {
                let t = text("text");
                self.text(&t, out);
                Block::Text(t.len())
            }
            "thinking" => {
                let t = text("thinking");
                if !t.is_empty() {
                    out.push(Ok(LlmEvent::ReasoningDelta(t.clone())));
                }
                Block::Thinking { text: t, signature: text("signature") }
            }
            "redacted_thinking" => Block::Redacted(text("data")),
            "tool_use" => {
                let call = self.tool_calls;
                self.tool_calls += 1;
                // Streamed input starts as `{}` and arrives in deltas; a gateway may send it whole.
                let input = block.get("input").filter(|i| i.as_object().is_some_and(|o| !o.is_empty())).map(Value::to_string);
                out.push(Ok(LlmEvent::ToolCallDelta {
                    index: call,
                    id: Some(text("id")),
                    name: Some(text("name")),
                    args_delta: input.clone().unwrap_or_default(),
                }));
                Block::Tool { index: call, id: text("id"), args: input.is_some() }
            }
            _ => Block::Other,
        };
        self.blocks.push((index, started));
    }

    fn delta(&mut self, index: u64, delta: &Value, out: &mut Vec<Result<LlmEvent, LlmError>>) {
        let Some((_, block)) = self.blocks.iter_mut().rev().find(|(i, _)| *i == index) else { return };
        let field = |key: &str| delta[key].as_str().unwrap_or_default().to_string();
        match (block, delta["type"].as_str().unwrap_or_default()) {
            (Block::Text(len), "text_delta") => {
                let t = field("text");
                *len += t.len();
                if !t.is_empty() {
                    self.text_seen = true;
                    out.push(Ok(LlmEvent::TextDelta(t)));
                }
            }
            (Block::Thinking { text, .. }, "thinking_delta") => {
                let t = field("thinking");
                text.push_str(&t);
                if !t.is_empty() {
                    out.push(Ok(LlmEvent::ReasoningDelta(t)));
                }
            }
            (Block::Thinking { signature, .. }, "signature_delta") => signature.push_str(&field("signature")),
            (Block::Tool { index, args, .. }, "input_json_delta") => {
                let part = field("partial_json");
                if !part.is_empty() {
                    *args = true;
                    out.push(Ok(LlmEvent::ToolCallDelta { index: *index, id: None, name: None, args_delta: part }));
                }
            }
            _ => {}
        }
    }

    fn text(&mut self, t: &str, out: &mut Vec<Result<LlmEvent, LlmError>>) {
        if !t.is_empty() {
            self.text_seen = true;
            out.push(Ok(LlmEvent::TextDelta(t.to_string())));
        }
    }

    fn finish_reason(&mut self, reason: &str, details: &Value, out: &mut Vec<Result<LlmEvent, LlmError>>) -> FinishReason {
        match reason {
            "tool_use" => FinishReason::ToolUse,
            "max_tokens" | "model_context_window_exceeded" => FinishReason::Length,
            "refusal" => {
                // A refusal is a normal end with nothing, or half an answer, in it; said so it is not taken for one.
                let why: Vec<&str> = [details["category"].as_str(), details["explanation"].as_str()].into_iter().flatten().collect();
                let why = if why.is_empty() { String::new() } else { format!(" ({})", why.join(": ")) };
                let gap = if self.text_seen { "\n\n" } else { "" };
                let note = format!("{gap}[The model declined to answer this request{why}.]");
                self.blocks.push((u64::MAX, Block::Text(note.len())));
                self.text(&note, out);
                FinishReason::Stop
            }
            // end_turn, stop_sequence, and pause_turn, which only server-side tools
            // cause; none are sent.
            _ => FinishReason::Stop,
        }
    }

    fn end(&mut self, out: &mut Vec<Result<LlmEvent, LlmError>>) {
        if self.done {
            return;
        }
        self.done = true;
        if let Some(replay) = self.replay() {
            out.push(Ok(LlmEvent::Replay(replay)));
        }
        if self.tokens.seen {
            let t = &self.tokens;
            out.push(Ok(LlmEvent::Usage(Usage {
                prompt: Some(t.input + t.written + t.read),
                completion: Some(t.output),
                cached: Some(t.read),
                mtp: None,
            })));
        }
        out.push(Ok(LlmEvent::Done(self.stop.unwrap_or(FinishReason::Stop))));
    }

    /// The signed thinking, verbatim, and where it stood among the text and
    /// the calls; `None` when the answer had no thinking to send back.
    fn replay(&self) -> Option<Value> {
        let signed = |b: &Block| matches!(b, Block::Thinking { signature, .. } if !signature.is_empty()) || matches!(b, Block::Redacted(_));
        if !self.blocks.iter().any(|(_, b)| signed(b)) {
            return None;
        }
        let blocks: Vec<Value> = self
            .blocks
            .iter()
            .filter_map(|(_, b)| match b {
                Block::Text(len) if *len > 0 => Some(json!({ "type": "text", "len": len })),
                Block::Thinking { text, signature } if !signature.is_empty() => Some(json!({ "type": "thinking", "thinking": text, "signature": signature })),
                Block::Redacted(data) => Some(json!({ "type": "redacted_thinking", "data": data })),
                Block::Tool { id, .. } => Some(json!({ "type": "tool_use", "id": id })),
                _ => None,
            })
            .collect();
        Some(json!({ "protocol": "anthropic", "blocks": blocks }))
    }
}

fn listed_model(v: &Value) -> Option<DiscoveredModel> {
    let id = v["id"].as_str()?.to_string();
    let f = family(&id);
    let caps = &v["capabilities"];
    let context = v["max_input_tokens"].as_u64().map(|n| n as usize).or((f.context > 0).then_some(f.context));
    let mut thinking = profile(&f);
    if caps["thinking"]["supported"] == false {
        thinking = ThinkingProfile::unsupported();
    } else if f.mode == Mode::Adaptive && caps["effort"].is_object() {
        thinking.presets.retain(|p| p == "off" || caps["effort"][p.as_str()]["supported"] != false);
        if thinking.default_preset.as_ref().is_some_and(|d| !thinking.presets.contains(d)) {
            thinking.default_preset = None;
        }
    }
    Some(DiscoveredModel {
        display_name: v["display_name"].as_str().map(str::to_string),
        is_loaded: false,
        context_length: context,
        max_context_length: context,
        thinking,
        supports_tools: true,
        supports_vision: caps["image_input"]["supported"].as_bool().unwrap_or(f.vision),
        id,
    })
}

async fn models_page(client: &Client, base: &str, url: &str) -> Option<Value> {
    let timeout = Duration::from_secs(5);
    if let Some(page) = client.get_json(url, &headers(client), timeout).await {
        return Some(page);
    }
    if is_official(base) || BEARER.lock().contains(base) {
        return None;
    }
    let page = client.get_json(url, &auth(client.api_key().as_deref(), true), timeout).await?;
    BEARER.lock().insert(base.to_string());
    Some(page)
}

pub(crate) async fn discover(client: &Client) -> Option<ServerDiscovery> {
    client.api_key()?;
    let base = client.base_url();
    let first = format!("{}/v1/models?limit=1000", root(&base));
    let mut url = first.clone();
    let mut models = Vec::new();
    // Bounded: a gateway that pages forever must not hold discovery up.
    for _ in 0..20 {
        let Some(page) = models_page(client, &base, &url).await else { break };
        let Some(data) = page["data"].as_array() else { break };
        models.extend(data.iter().filter_map(listed_model));
        match page["last_id"].as_str().filter(|_| page["has_more"] == true) {
            Some(last) => url = format!("{first}&after_id={last}"),
            None => break,
        }
    }
    client.settle_discovery(models, ServerKind::Anthropic)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ApiProtocol, Endpoint};
    use crate::LlmBackend;
    use futures::StreamExt;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn client(url: &str, model: &str) -> Client {
        Client::new(Endpoint::new(ApiProtocol::Anthropic, url, Some("k".into())), model)
    }

    fn request(model: &str, messages: &[ChatMessage], tools: &[ToolSpec], options: &TurnOptions) -> Value {
        let llm = client("https://api.anthropic.com", model);
        let plan = plan(&llm, model, messages, options);
        body(model, messages, tools, options, &plan)
    }

    fn effort(e: &str) -> TurnOptions {
        TurnOptions { custom_effort: Some(e.into()), ..Default::default() }
    }

    fn shell() -> ToolSpec {
        ToolSpec {
            name: "run_shell".into(),
            description: "Run a command".into(),
            parameters_json: r#"{"type":"object","properties":{"command":{"type":"string"}}}"#.into(),
        }
    }

    fn calling(calls: &[(&str, &str)]) -> ChatMessage {
        let mut m = ChatMessage::assistant("");
        m.tool_calls = calls.iter().map(|(id, args)| ToolCall { id: id.to_string(), name: "run_shell".into(), args_json: args.to_string() }).collect();
        m
    }

    fn signed(signature: &str) -> Value {
        json!({ "type": "thinking", "thinking": "plan", "signature": signature })
    }

    fn replay(blocks: Vec<Value>) -> Option<Value> {
        Some(json!({ "protocol": "anthropic", "blocks": blocks }))
    }

    fn kinds(blocks: &Value) -> Vec<&str> {
        blocks.as_array().unwrap().iter().map(|b| b["type"].as_str().unwrap()).collect()
    }

    #[test]
    fn messages_alternate_and_tool_results_share_one_user_message() {
        let mut picture = ChatMessage::user("[Image opened by view_image and shown below]");
        picture.images.push("data:image/png;base64,AAAA".into());
        let messages = vec![
            ChatMessage::system("You are FlashAgent."),
            ChatMessage::user("first"),
            ChatMessage::user("second"),
            calling(&[("toolu_1", r#"{"command":"ls"}"#), ("toolu_2", "{'command': 'pwd',}")]),
            ChatMessage::tool_result("toolu_1", "a.rs"),
            ChatMessage::tool_result("toolu_2", ""),
            picture,
            ChatMessage::assistant("   "),
            ChatMessage::user("thanks"),
        ];
        let body = request("claude-opus-4-8", &messages, &[], &TurnOptions::default());
        let turns = body["messages"].as_array().unwrap();
        let roles: Vec<&str> = turns.iter().map(|t| t["role"].as_str().unwrap()).collect();
        assert_eq!(roles, ["user", "assistant", "user"], "a blank answer is left out and the user messages around it join");
        assert_eq!(kinds(&turns[0]["content"]), ["text", "text"]);
        assert_eq!(turns[1]["content"][1]["input"], json!({ "command": "pwd" }), "broken arguments are repaired into an object");
        let results = &turns[2]["content"];
        assert_eq!(kinds(results), ["tool_result", "tool_result", "text", "image", "text"]);
        assert_eq!(results[0]["tool_use_id"], "toolu_1");
        assert_eq!(results[0]["content"], "a.rs");
        assert!(results[1].get("content").is_none(), "an empty result sends no empty content");
        assert_eq!(results[3]["source"], json!({ "type": "base64", "media_type": "image/png", "data": "AAAA" }));
        assert_eq!(body["system"][0]["text"], "You are FlashAgent.");
    }

    #[test]
    fn a_result_without_its_call_becomes_text_and_a_call_without_a_result_gets_one() {
        let messages = vec![
            ChatMessage::tool_result("gone", "old output"),
            ChatMessage::user("go on"),
            calling(&[("call.7", "{}")]),
            ChatMessage::user("never mind"),
        ];
        let body = request("claude-sonnet-4-5", &messages, &[], &TurnOptions::default());
        let turns = &body["messages"];
        assert_eq!(turns[0]["content"][0], json!({ "type": "text", "text": "Result of tool call gone:\nold output" }));
        assert_eq!(turns[1]["content"][0]["id"], "call_7", "an id another provider wrote is made acceptable");
        assert_eq!(turns[2]["content"][0]["type"], "tool_result");
        assert_eq!(turns[2]["content"][0]["tool_use_id"], "call_7");
        assert_eq!(turns[2]["content"][0]["is_error"], true);
        assert_eq!(turns[2]["content"][1]["text"], "never mind");
    }

    #[test]
    fn only_pictures_the_api_reads_are_sent() {
        assert_eq!(image("data:image/jpg;base64,QQ==").unwrap()["source"]["media_type"], "image/jpeg");
        assert_eq!(image("https://example.com/a.png").unwrap()["source"], json!({ "type": "url", "url": "https://example.com/a.png" }));
        assert!(image("data:image/bmp;base64,QQ==").is_none());
        assert!(image("data:image/png,raw").is_none());
    }

    #[test]
    fn signed_thinking_goes_back_first_and_another_protocol_s_replay_is_ignored() {
        let mut claude = calling(&[("toolu_1", "{}")]);
        claude.content = "Checking.".into();
        claude.replay = replay(vec![signed("sig-first"), json!({ "type": "redacted_thinking", "data": "opaque-first" })]);
        let mut gemini = ChatMessage::assistant("Done.");
        gemini.reasoning = Some("thought".into());
        gemini.replay = Some(json!({ "protocol": "gemini", "signature": "xyz" }));
        let messages = vec![ChatMessage::user("fix it"), claude, ChatMessage::tool_result("toolu_1", "ok"), gemini, ChatMessage::user("next")];
        let body = request("claude-opus-4-8", &messages, &[], &TurnOptions::default());
        assert_eq!(kinds(&body["messages"][1]["content"]), ["thinking", "redacted_thinking", "text", "tool_use"]);
        assert_eq!(body["messages"][1]["content"][0], signed("sig-first"), "sent back exactly as it came");
        assert_eq!(body["messages"][3]["content"], json!([{ "type": "text", "text": "Done." }]));

        let old = request("claude-3-5-haiku-20241022", &messages, &[], &TurnOptions::default());
        assert_eq!(kinds(&old["messages"][1]["content"]), ["text", "tool_use"], "a model that cannot think is sent no thinking");
    }

    #[test]
    fn thinking_between_calls_goes_back_where_it_stood() {
        let mut m = calling(&[("toolu_1", r#"{"command":"ls"}"#), ("toolu_2", r#"{"command":"pwd"}"#)]);
        m.content = "Let me look.Now the other.".into();
        m.replay = replay(vec![
            signed("sig-between-1"),
            json!({ "type": "text", "len": 12 }),
            json!({ "type": "tool_use", "id": "toolu_1" }),
            signed("sig-between-2"),
            json!({ "type": "text", "len": 14 }),
            json!({ "type": "tool_use", "id": "toolu_2" }),
        ]);
        let history = |m: &ChatMessage| {
            vec![ChatMessage::user("look"), m.clone(), ChatMessage::tool_result("toolu_1", "a"), ChatMessage::tool_result("toolu_2", "b")]
        };
        let body = request("claude-opus-5", &history(&m), &[], &TurnOptions::default());
        let blocks = &body["messages"][1]["content"];
        assert_eq!(kinds(blocks), ["thinking", "text", "tool_use", "thinking", "text", "tool_use"]);
        assert_eq!(blocks[1]["text"], "Let me look.");
        assert_eq!(blocks[4]["text"], "Now the other.");

        m.content.push_str(" And more.");
        let body = request("claude-opus-5", &history(&m), &[], &TurnOptions::default());
        assert_eq!(kinds(&body["messages"][1]["content"]), ["thinking", "thinking", "text", "tool_use", "tool_use"], "text the loop changed puts thinking first");
    }

    #[test]
    fn cache_breakpoints_sit_on_the_last_tool_the_system_prompt_and_the_newest_block() {
        let tools = [shell(), ToolSpec { name: "read_file".into(), ..shell() }];
        let messages = [ChatMessage::system("S"), ChatMessage::user("a"), ChatMessage::assistant("b"), ChatMessage::user("c")];
        let body = request("claude-sonnet-5", &messages, &tools, &TurnOptions::default());
        assert_eq!(body.to_string().matches("cache_control").count(), 3, "{body}");
        assert!(body["tools"][0].get("cache_control").is_none());
        assert_eq!(body["tools"][1]["cache_control"], json!({ "type": "ephemeral" }));
        assert_eq!(body["system"][0]["cache_control"], json!({ "type": "ephemeral" }));
        assert_eq!(body["messages"][2]["content"][0]["cache_control"], json!({ "type": "ephemeral" }));
        assert_eq!(body["tools"][0]["input_schema"]["properties"]["command"]["type"], "string");
        assert_eq!(body["tools"][0]["eager_input_streaming"], true);
    }

    #[test]
    fn the_system_prompt_is_the_same_bytes_whatever_the_thinking() {
        let sys = "You are FlashAgent.\n  Keep this exactly.  ";
        let messages = [ChatMessage::system(sys), ChatMessage::user("hi")];
        for e in ["off", "low", "xhigh"] {
            let body = request("claude-opus-5", &messages, &[], &effort(e));
            assert_eq!(body["system"], json!([{ "type": "text", "text": sys, "cache_control": { "type": "ephemeral" } }]), "{e}");
        }
    }

    #[test]
    fn fields_the_api_refuses_are_never_sent() {
        let options = TurnOptions {
            temperature: Some(1.4),
            top_p: Some(0.9),
            top_k: Some(40),
            repeat_penalty: Some(1.1),
            presence_penalty: Some(0.5),
            min_p: Some(0.05),
            custom_effort: Some("off".into()),
            ..Default::default()
        };
        let haiku = request("claude-haiku-4-5", &[ChatMessage::user("hi")], &[], &options);
        assert_eq!(haiku["temperature"], 1.0, "held to the API's range");
        assert!(haiku.get("top_p").is_none(), "some models refuse temperature and top_p together");
        assert_eq!(haiku["top_k"], 40);
        for field in ["repeat_penalty", "repetition_penalty", "presence_penalty", "min_p", "thinking"] {
            assert!(haiku.get(field).is_none(), "{field}");
        }
        let thinking = request("claude-haiku-4-5", &[ChatMessage::user("hi")], &[], &TurnOptions { custom_effort: Some("high".into()), ..options.clone() });
        assert!(thinking.get("temperature").is_none() && thinking.get("top_k").is_none(), "sampling does not go with thinking");
        let opus = request("claude-opus-4-8", &[ChatMessage::user("hi")], &[], &options);
        assert!(opus.get("temperature").is_none() && opus.get("top_k").is_none(), "removed on the newest models");
    }

    #[test]
    fn each_model_gets_thinking_in_the_form_it_takes() {
        let ask = |model: &str, e: &str| {
            let body = request(model, &[ChatMessage::user("hi")], &[], &effort(e));
            (body["thinking"].clone(), body["output_config"]["effort"].clone())
        };
        let summarized = json!({ "type": "adaptive", "display": "summarized" });
        assert_eq!(ask("claude-opus-4-8", "xhigh"), (summarized.clone(), json!("xhigh")));
        assert_eq!(ask("claude-opus-4-8", "off"), (json!({ "type": "disabled" }), Value::Null));
        assert_eq!(ask("claude-opus-5", "off"), (json!({ "type": "disabled" }), Value::Null), "no effort, so never above high");
        assert_eq!(ask("claude-opus-4-6", "xhigh"), (json!({ "type": "adaptive" }), json!("high")), "4.6 has no xhigh and no display");
        assert_eq!(ask("claude-opus-5-5", "off"), (summarized.clone(), json!("low")), "it cannot stop thinking, so it thinks least");
        assert_eq!(ask("claude-fable-5-1", "none"), (summarized.clone(), json!("low")));
        assert_eq!(ask("claude-sonnet-5", "on"), (summarized, Value::Null));
        assert_eq!(ask("claude-sonnet-4-5-20250929", "medium"), (json!({ "type": "enabled", "budget_tokens": 12288 }), Value::Null));
        assert_eq!(ask("claude-sonnet-4-5", "off").0, Value::Null);
        assert_eq!(ask("claude-3-5-haiku-20241022", "high").0, Value::Null, "a model without extended thinking gets none");

        let preset = |model: &str, thinking: ThinkingEffort| {
            let llm = client("https://api.anthropic.com", model).with_profile(profile(&family(model)));
            let options = TurnOptions { thinking, ..Default::default() };
            let messages = [ChatMessage::user("hi")];
            body(model, &messages, &[], &options, &plan(&llm, model, &messages, &options))["output_config"]["effort"].clone()
        };
        assert_eq!(preset("claude-opus-4-8", ThinkingEffort::High), "xhigh", "the most the model offers");
        assert_eq!(preset("claude-opus-5-5", ThinkingEffort::Off), "low");
        assert_eq!(preset("claude-opus-5-5", ThinkingEffort::Default), "medium");
    }

    #[test]
    fn auto_leaves_the_depth_to_adaptive_thinking_so_a_new_prompt_keeps_the_cache() {
        let greeting = request("claude-opus-5", &[ChatMessage::user("hi")], &[], &TurnOptions::default());
        let task = request("claude-opus-5", &[ChatMessage::user("Refactor the parser in src/parse.rs and cover every edge case with tests")], &[], &TurnOptions::default());
        assert_eq!(greeting["thinking"], task["thinking"]);
        assert!(greeting.get("output_config").is_none() && task.get("output_config").is_none());
    }

    #[test]
    fn budget_thinking_switched_inside_a_tool_loop_waits_for_the_next_prompt() {
        let so_far = vec![ChatMessage::user("list files"), calling(&[("toolu_1", "{}")]), ChatMessage::tool_result("toolu_1", "a.rs")];
        let on = request("claude-haiku-4-5", &so_far, &[], &effort("high"));
        assert!(on.get("thinking").is_none(), "the call being answered did not start with thinking");
        let mut next = so_far.clone();
        next.push(ChatMessage::assistant("There is one file."));
        next.push(ChatMessage::user("now read it"));
        assert_eq!(request("claude-haiku-4-5", &next, &[], &effort("high"))["thinking"]["type"], "enabled");

        let mut thought = calling(&[("toolu_1", "{}")]);
        thought.replay = replay(vec![signed("sig-mid-loop"), json!({ "type": "tool_use", "id": "toolu_1" })]);
        let history = [ChatMessage::user("list files"), thought, ChatMessage::tool_result("toolu_1", "a.rs")];
        let off = request("claude-haiku-4-5", &history, &[], &effort("off"));
        assert_eq!(kinds(&off["messages"][1]["content"]), ["tool_use"], "switched off: the open call carries no thinking");
        let still_on = request("claude-haiku-4-5", &history, &[], &effort("high"));
        assert_eq!(kinds(&still_on["messages"][1]["content"]), ["thinking", "tool_use"]);
        assert_eq!(still_on["thinking"]["type"], "enabled");
    }

    #[test]
    fn max_tokens_always_leaves_room_above_the_thinking_budget() {
        let opus41 = request("claude-opus-4-1", &[ChatMessage::user("hi")], &[], &effort("high"));
        assert_eq!(opus41["max_tokens"], 32000, "Opus 4.1 writes at most 32k");
        assert_eq!(opus41["thinking"]["budget_tokens"], 24576);
        let small = request("claude-sonnet-4-5", &[ChatMessage::user("hi")], &[], &TurnOptions { max_tokens: Some(4000), ..effort("high") });
        assert_eq!(small["max_tokens"], 4000);
        assert_eq!(small["thinking"]["budget_tokens"], 2976);
        let warm_up = request("claude-sonnet-4-5", &[ChatMessage::user("hi")], &[], &TurnOptions { max_tokens: Some(1), ..effort("high") });
        assert!(warm_up.get("thinking").is_none(), "a one-token request cannot hold a budget");
        assert_eq!(request("claude-opus-5", &[ChatMessage::user("hi")], &[], &TurnOptions::default())["max_tokens"], 64000);
        assert_eq!(request("glm-4.6", &[ChatMessage::user("hi")], &[], &TurnOptions::default())["max_tokens"], 8192, "an unknown model's limit is unknown");
    }

    #[test]
    fn presets_follow_the_model_family() {
        let presets = |m: &str| profile(&family(m)).presets.join(",");
        assert_eq!(presets("claude-opus-5"), "off,low,medium,high,xhigh,max");
        assert_eq!(presets("claude-opus-5-5"), "low,medium,high,xhigh,max");
        assert_eq!(presets("claude-fable-5-1"), "low,medium,high,xhigh,max");
        assert_eq!(presets("claude-sonnet-4-6"), "off,low,medium,high,max");
        assert_eq!(presets("claude-haiku-4-5-20251001"), "off,low,medium,high");
        assert!(!profile(&family("claude-3-haiku-20240307")).supported);
        assert_eq!(profile(&family("claude-opus-5-5")).default_preset.as_deref(), Some("medium"));
        assert_eq!(profile(&family("claude-opus-5")).default_preset.as_deref(), Some("high"));
        assert_eq!(profile(&family("claude-opus-4-8")).default_preset.as_deref(), Some("off"), "it does not think unless asked");
        assert_eq!(profile(&family("claude-opus-5")).protocol, ThinkingProtocol::Anthropic);
        assert_eq!(family("us.anthropic.claude-opus-4-1-20250805-v1:0").max_output, 32_000);
        assert_eq!(family("claude-opus-4-20250514").max_output, 32_000);
        assert_eq!(family("anthropic/claude-sonnet-4.5").mode, Mode::Budget);
        assert_eq!(family("claude-3-7-sonnet-20250219").mode, Mode::Budget);
        assert_eq!(family("claude-sonnet-4-6").context, 1_000_000);
        assert_eq!(family("claude-haiku-4-5").context, 200_000);
        assert!(!family("claude-mythos-preview").can_disable);
    }

    #[test]
    fn every_way_of_writing_the_address_reaches_the_same_api() {
        for base in ["https://api.anthropic.com", "https://api.anthropic.com/v1", "https://api.anthropic.com/v1/messages", ""] {
            assert_eq!(root(base), "https://api.anthropic.com", "{base:?}");
            assert!(is_official(base), "{base:?}");
        }
        assert_eq!(root("https://openrouter.ai/api"), "https://openrouter.ai/api");
        assert!(!is_official("https://openrouter.ai/api"));
    }

    #[test]
    fn an_answer_cut_off_while_thinking_is_not_sent_back() {
        let mut cut = ChatMessage::assistant("");
        cut.replay = replay(vec![signed("sig-cut-off")]);
        let body = request("claude-opus-5", &[ChatMessage::user("q"), cut, ChatMessage::user("again")], &[], &TurnOptions::default());
        assert_eq!(body["messages"].as_array().unwrap().len(), 1, "no assistant message holding only thinking");
        assert_eq!(kinds(&body["messages"][0]["content"]), ["text", "text"]);
    }

    #[test]
    fn what_a_refusal_names_is_read_from_its_text() {
        assert_eq!(context_room("input length and `max_tokens` exceed context limit: 188240 + 21333 > 200000, decrease input length"), Some(11760));
        assert_eq!(named_cap("max_tokens: 64000 > 32000, which is the maximum allowed number of output tokens for claude-opus-4-1-20250805", 64000), Some(32000));
        assert_eq!(named_cap("invalid max_tokens value, the valid range of max_tokens is [1, 8192]", 64000), Some(8192));
        assert_eq!(named_cap("`max_tokens` must be greater than `thinking.budget_tokens` (24576)", 64000), None);
        assert_eq!(block_path("messages.5.content.2: Invalid `signature` in `thinking` block"), Some((5, 2)));
        assert_eq!(error_message(r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad"}}"#), "bad");
    }

    fn sse(events: &[Value]) -> String {
        events.iter().map(|e| format!("event: {}\r\ndata: {e}\r\n\r\n", e["type"].as_str().unwrap())).collect()
    }

    fn opening(usage: Value) -> Value {
        json!({ "type": "message_start", "message": { "id": "msg_1", "type": "message", "role": "assistant", "content": [], "usage": usage } })
    }

    fn start(index: u64, block: Value) -> Value {
        json!({ "type": "content_block_start", "index": index, "content_block": block })
    }

    fn delta(index: u64, delta: Value) -> Value {
        json!({ "type": "content_block_delta", "index": index, "delta": delta })
    }

    fn stop(index: u64) -> Value {
        json!({ "type": "content_block_stop", "index": index })
    }

    fn ending(reason: &str, output: i64) -> Value {
        json!({ "type": "message_delta", "delta": { "stop_reason": reason, "stop_sequence": null }, "usage": { "output_tokens": output } })
    }

    fn text_delta(t: &str) -> Value {
        json!({ "type": "text_delta", "text": t })
    }

    fn decode(transcript: &str, chunk: usize) -> Vec<Result<LlmEvent, LlmError>> {
        let mut decoder = Decoder::default();
        let mut out = Vec::new();
        for piece in transcript.as_bytes().chunks(chunk) {
            out.extend(decoder.feed(piece));
        }
        out.extend(decoder.finish());
        out
    }

    fn events(transcript: &str, chunk: usize) -> Vec<LlmEvent> {
        decode(transcript, chunk).into_iter().map(Result::unwrap).collect()
    }

    fn said(events: &[LlmEvent]) -> String {
        events.iter().filter_map(|e| if let LlmEvent::TextDelta(t) = e { Some(t.as_str()) } else { None }).collect()
    }

    fn answer(text: &str) -> String {
        sse(&[
            opening(json!({ "input_tokens": 5, "output_tokens": 1 })),
            start(0, json!({ "type": "text", "text": "" })),
            delta(0, text_delta(text)),
            stop(0),
            ending("end_turn", 3),
            json!({ "type": "message_stop" }),
        ])
    }

    #[test]
    fn a_stream_split_anywhere_decodes_the_same() {
        let transcript = sse(&[
            opening(json!({ "input_tokens": 5 })),
            start(0, json!({ "type": "text", "text": "" })),
            delta(0, text_delta("Grüße, ")),
            json!({ "type": "ping" }),
            delta(0, text_delta("world")),
            stop(0),
            ending("end_turn", 3),
            json!({ "type": "message_stop" }),
        ]);
        let whole = events(&transcript, transcript.len());
        for chunk in [1, 2, 3, 7] {
            assert_eq!(events(&transcript, chunk), whole, "chunks of {chunk} bytes");
        }
        assert_eq!(events(&transcript.replace("\r\n", "\n"), 5), whole, "plain line feeds");
        assert_eq!(said(&whole), "Grüße, world");
        assert_eq!(whole.last(), Some(&LlmEvent::Done(FinishReason::Stop)));
    }

    #[test]
    fn thinking_is_shown_and_comes_back_signed_in_the_replay() {
        let transcript = sse(&[
            opening(json!({ "input_tokens": 5 })),
            start(0, json!({ "type": "thinking", "thinking": "", "signature": "" })),
            delta(0, json!({ "type": "thinking_delta", "thinking": "Let me " })),
            delta(0, json!({ "type": "thinking_delta", "thinking": "think." })),
            delta(0, json!({ "type": "signature_delta", "signature": "sig-streamed" })),
            stop(0),
            start(1, json!({ "type": "redacted_thinking", "data": "opaque-streamed" })),
            stop(1),
            start(2, json!({ "type": "text", "text": "" })),
            delta(2, text_delta("Answer.")),
            stop(2),
            ending("end_turn", 9),
            json!({ "type": "message_stop" }),
        ]);
        let out = events(&transcript, 5);
        let reasoning: String = out.iter().filter_map(|e| if let LlmEvent::ReasoningDelta(t) = e { Some(t.as_str()) } else { None }).collect();
        assert_eq!(reasoning, "Let me think.");
        let state = out.iter().find_map(|e| if let LlmEvent::Replay(v) = e { Some(v.clone()) } else { None }).expect("a replay");
        let thought = json!({ "type": "thinking", "thinking": "Let me think.", "signature": "sig-streamed" });
        let redacted = json!({ "type": "redacted_thinking", "data": "opaque-streamed" });
        assert_eq!(state, json!({ "protocol": "anthropic", "blocks": [thought, redacted, { "type": "text", "len": 7 }] }));

        let mut m = ChatMessage::assistant(said(&out));
        m.replay = Some(state);
        let body = request("claude-opus-5", &[ChatMessage::user("q"), m, ChatMessage::user("more")], &[], &TurnOptions::default());
        assert_eq!(body["messages"][1]["content"], json!([thought, redacted, { "type": "text", "text": "Answer." }]));
    }

    #[test]
    fn tool_calls_are_counted_from_zero_and_their_arguments_arrive_in_pieces() {
        let tool = |id: &str, name: &str| json!({ "type": "tool_use", "id": id, "name": name, "input": {} });
        let args = |part: &str| json!({ "type": "input_json_delta", "partial_json": part });
        let transcript = sse(&[
            opening(json!({ "input_tokens": 5 })),
            start(0, json!({ "type": "text", "text": "I'll look." })),
            stop(0),
            start(1, tool("toolu_a", "run_shell")),
            delta(1, args("")),
            delta(1, args("{\"comm")),
            delta(1, args("and\": \"ls\"}")),
            stop(1),
            start(2, tool("toolu_b", "list_dir")),
            stop(2),
            ending("tool_use", 20),
            json!({ "type": "message_stop" }),
        ]);
        let out = events(&transcript, 11);
        assert_eq!(said(&out), "I'll look.");
        let calls: Vec<(usize, Option<String>, Option<String>, String)> = out
            .iter()
            .filter_map(|e| match e {
                LlmEvent::ToolCallDelta { index, id, name, args_delta } => Some((*index, id.clone(), name.clone(), args_delta.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(calls[0], (0, Some("toolu_a".into()), Some("run_shell".into()), String::new()), "the first fragment names the call");
        assert!(calls[1..].iter().all(|(_, id, name, _)| id.is_none() || name.as_deref() == Some("list_dir")));
        let joined = |i: usize| calls.iter().filter(|c| c.0 == i).map(|c| c.3.as_str()).collect::<String>();
        assert_eq!(joined(0), r#"{"command": "ls"}"#);
        assert_eq!(joined(1), "{}", "a call that streamed no input gets an empty object");
        assert_eq!(out.last(), Some(&LlmEvent::Done(FinishReason::ToolUse)));
    }

    #[test]
    fn each_stop_reason_ends_the_turn_the_right_way() {
        for (reason, want) in [
            ("end_turn", FinishReason::Stop),
            ("stop_sequence", FinishReason::Stop),
            ("tool_use", FinishReason::ToolUse),
            ("max_tokens", FinishReason::Length),
            ("model_context_window_exceeded", FinishReason::Length),
            ("pause_turn", FinishReason::Stop),
            ("refusal", FinishReason::Stop),
        ] {
            let transcript = sse(&[opening(json!({ "input_tokens": 1 })), ending(reason, 1), json!({ "type": "message_stop" })]);
            assert_eq!(events(&transcript, 64).last(), Some(&LlmEvent::Done(want)), "{reason}");
        }
    }

    #[test]
    fn a_refusal_is_said_in_the_answer() {
        let transcript = sse(&[
            opening(json!({ "input_tokens": 1 })),
            start(0, json!({ "type": "text", "text": "" })),
            delta(0, text_delta("Sure, the exploit")),
            stop(0),
            json!({ "type": "message_delta", "delta": { "stop_reason": "refusal", "stop_details": { "type": "refusal", "category": "cyber", "explanation": null } }, "usage": { "output_tokens": 4 } }),
            json!({ "type": "message_stop" }),
        ]);
        let text = said(&events(&transcript, 64));
        assert!(text.starts_with("Sure, the exploit\n\n[The model declined"), "{text}");
        assert!(text.contains("(cyber)"), "{text}");
    }

    #[test]
    fn usage_counts_cache_reads_and_writes_in_the_prompt() {
        let transcript = sse(&[
            opening(json!({ "input_tokens": 20, "cache_creation_input_tokens": 300, "cache_read_input_tokens": 4000, "output_tokens": 1 })),
            ending("end_turn", 42),
            json!({ "type": "message_stop" }),
        ]);
        let usage: Vec<Usage> = events(&transcript, 64).into_iter().filter_map(|e| if let LlmEvent::Usage(u) = e { Some(u) } else { None }).collect();
        assert_eq!(usage, [Usage { prompt: Some(4320), completion: Some(42), cached: Some(4000), mtp: None }], "once, at the end: the loop adds every report up");
    }

    #[test]
    fn an_error_event_ends_the_stream_with_its_message() {
        let transcript = sse(&[
            opening(json!({ "input_tokens": 1 })),
            start(0, json!({ "type": "text", "text": "" })),
            delta(0, text_delta("par")),
            json!({ "type": "error", "error": { "type": "overloaded_error", "message": "Overloaded" } }),
        ]);
        let out = decode(&transcript, 16);
        assert!(matches!(out.last(), Some(Err(LlmError::Stream(m))) if m == "Overloaded"), "{out:?}");
        assert!(!out.iter().any(|e| matches!(e, Ok(LlmEvent::Done(_)))));
    }

    #[test]
    fn a_stream_cut_short_still_reports_what_it_had() {
        let transcript = sse(&[opening(json!({ "input_tokens": 7 })), start(0, json!({ "type": "text", "text": "" })), delta(0, text_delta("half"))]);
        let out = events(&transcript[..transcript.len() - 2], 9);
        assert_eq!(said(&out), "half");
        assert!(matches!(out.last(), Some(LlmEvent::Done(FinishReason::Stop))));
    }

    /// Reads one HTTP request, headers and body.
    async fn read_request(sock: &mut tokio::net::TcpStream) -> String {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 16384];
        loop {
            let n = sock.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
                let len = head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
                if buf.len() >= end + 4 + len {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&buf).to_string()
    }

    type Seen = Arc<std::sync::Mutex<Vec<String>>>;

    /// Answers every request with what `answer` makes of it, and keeps them all.
    async fn serve(answer: impl Fn(&str) -> (&'static str, String) + Send + 'static) -> (String, Seen) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Seen = Default::default();
        let log = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let req = read_request(&mut sock).await;
                let (status, body) = answer(&req);
                log.lock().unwrap().push(req);
                let resp = format!("HTTP/1.1 {status}\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        (format!("http://{addr}"), seen)
    }

    async fn collect(llm: &Client, messages: &[ChatMessage]) -> Vec<LlmEvent> {
        let mut stream = llm.stream_with_options(messages, &[shell()], &TurnOptions::default()).await.expect("the request went through");
        let mut out = Vec::new();
        while let Some(item) = stream.next().await {
            out.push(item.unwrap());
        }
        out
    }

    fn sent_body(req: &str) -> Value {
        serde_json::from_str(req.split("\r\n\r\n").nth(1).unwrap_or_default()).unwrap()
    }

    #[tokio::test]
    async fn a_turn_is_posted_to_v1_messages_with_the_key_and_the_api_version() {
        let (url, seen) = serve(|_| ("200 OK", answer("Hi"))).await;
        for base in [url.clone(), format!("{url}/v1")] {
            let llm = client(&base, "claude-opus-4-8");
            let out = collect(&llm, &[ChatMessage::system("S"), ChatMessage::user("hi")]).await;
            assert_eq!(said(&out), "Hi");
            assert!(out.iter().any(|e| matches!(e, LlmEvent::Usage(u) if u.prompt == Some(5) && u.completion == Some(3))));
            assert_eq!(out.last(), Some(&LlmEvent::Done(FinishReason::Stop)));
            assert_eq!(llm.requests_in_flight(), 0);
            assert!(llm.profile().is_some_and(|p| p.presets.contains(&"xhigh".to_string())), "without a model list the presets still follow the model");
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        for req in seen.iter() {
            let head = req.split("\r\n\r\n").next().unwrap().to_ascii_lowercase();
            assert!(head.starts_with("post /v1/messages http/1.1\r\n"), "{head}");
            assert!(head.contains("\r\nx-api-key: k\r\n"), "{head}");
            assert!(head.contains("\r\nanthropic-version: 2023-06-01\r\n"), "{head}");
            assert!(!head.contains("authorization:"), "the key goes in x-api-key alone");
            let body = sent_body(req);
            assert_eq!((body["model"].as_str(), body["stream"].as_bool()), (Some("claude-opus-4-8"), Some(true)));
            assert!(body["tools"][0].get("eager_input_streaming").is_none(), "only the real API is sure to take it");
        }
    }

    #[tokio::test]
    async fn a_refused_request_keeps_anthropic_s_error_body() {
        let error = r#"{"type":"error","error":{"type":"invalid_request_error","message":"messages: at least one message is required"}}"#;
        let (url, seen) = serve(move |_| ("400 Bad Request", error.to_string())).await;
        let llm = client(&url, "claude-opus-5");
        let res = llm.stream(&[ChatMessage::user("hi")], &[]).await;
        assert!(matches!(res, Err(LlmError::Status { status: 400, ref body }) if body == error));
        assert_eq!(seen.lock().unwrap().len(), 1, "it names nothing to correct, so it is not sent again");
        assert_eq!(llm.requests_in_flight(), 0);
    }

    #[tokio::test]
    async fn a_gateway_that_wants_a_bearer_token_gets_one_from_then_on() {
        let (url, seen) = serve(|req| {
            if req.to_ascii_lowercase().contains("\r\nauthorization: bearer k\r\n") {
                ("200 OK", answer("ok"))
            } else {
                ("401 Unauthorized", r#"{"error":{"message":"invalid token"}}"#.to_string())
            }
        })
        .await;
        let llm = client(&url, "claude-sonnet-5");
        for _ in 0..2 {
            assert_eq!(said(&collect(&llm, &[ChatMessage::user("hi")]).await), "ok");
        }
        assert_eq!(seen.lock().unwrap().len(), 3, "refused once, then asked the right way");
    }

    #[tokio::test]
    async fn a_thinking_block_bound_to_a_changed_history_is_left_out_from_then_on() {
        let refusal = r#"{"type":"error","error":{"type":"invalid_request_error","message":"messages.3.content.0: Invalid `signature` in `thinking` block. The block is bound to a different conversation."}}"#;
        let (url, seen) = serve(move |req| if req.contains("sig-rebound") { ("400 Bad Request", refusal.to_string()) } else { ("200 OK", answer("ok")) }).await;
        let mut before = ChatMessage::assistant("Before.");
        before.replay = replay(vec![signed("sig-kept-before")]);
        let mut rebound = ChatMessage::assistant("Done.");
        rebound.replay = replay(vec![signed("sig-rebound")]);
        let history = [ChatMessage::user("one"), before, ChatMessage::user("two"), rebound, ChatMessage::user("three")];
        let llm = client(&url, "claude-opus-5-5");
        for _ in 0..2 {
            assert_eq!(said(&collect(&llm, &history).await), "ok");
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3, "refused once, then left out without asking");
        let last = sent_body(seen.last().unwrap());
        assert_eq!(kinds(&last["messages"][1]["content"]), ["thinking", "text"], "the block before the one named is still valid");
        assert_eq!(kinds(&last["messages"][3]["content"]), ["text"]);
    }

    #[tokio::test]
    async fn an_output_cap_the_server_names_is_used_from_then_on() {
        let refusal = r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: 64000 > 21333, which is the maximum allowed number of output tokens for claude-opus-4-8"}}"#;
        let (url, seen) = serve(move |req| if req.contains("\"max_tokens\":64000") { ("400 Bad Request", refusal.to_string()) } else { ("200 OK", answer("ok")) }).await;
        let llm = client(&url, "claude-opus-4-8");
        for _ in 0..2 {
            assert_eq!(said(&collect(&llm, &[ChatMessage::user("hi")]).await), "ok");
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert_eq!(sent_body(seen.last().unwrap())["max_tokens"], 21333);
    }

    #[tokio::test]
    async fn discovery_reads_every_page_of_the_model_list() {
        let (url, seen) = serve(|req| {
            let page = if req.starts_with("GET /v1/models?limit=1000&after_id=claude-opus-4-8 ") {
                json!({ "data": [{ "type": "model", "id": "claude-haiku-4-5-20251001", "display_name": "Claude Haiku 4.5" }], "has_more": false, "last_id": "claude-haiku-4-5-20251001" })
            } else {
                json!({
                    "data": [
                        {
                            "type": "model", "id": "claude-opus-5", "display_name": "Claude Opus 5", "max_input_tokens": 1_000_000, "max_tokens": 128_000,
                            "capabilities": {
                                "image_input": { "supported": true },
                                "thinking": { "supported": true },
                                "effort": { "supported": true, "low": { "supported": true }, "medium": { "supported": true }, "high": { "supported": true }, "xhigh": { "supported": true }, "max": { "supported": false } }
                            }
                        },
                        { "type": "model", "id": "claude-opus-4-8", "display_name": "Claude Opus 4.8", "max_input_tokens": 500_000 }
                    ],
                    "has_more": true,
                    "last_id": "claude-opus-4-8"
                })
            };
            ("200 OK", page.to_string())
        })
        .await;
        let llm = client(&url, "claude-haiku-4-5");
        let disc = llm.discover_server().await.expect("the list was read");
        assert_eq!(disc.kind, ServerKind::Anthropic);
        let ids: Vec<&str> = disc.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["claude-opus-5", "claude-opus-4-8", "claude-haiku-4-5-20251001"]);
        assert_eq!(llm.model(), "claude-haiku-4-5-20251001", "the configured alias finds its dated id");
        let opus = &disc.models[0];
        assert_eq!((opus.context_length, opus.display_name.as_deref(), opus.supports_vision, opus.is_loaded), (Some(1_000_000), Some("Claude Opus 5"), true, false));
        assert_eq!(opus.thinking.presets.join(","), "off,low,medium,high,xhigh", "a level the API says is unsupported is not offered");
        assert_eq!(opus.thinking.protocol, ThinkingProtocol::Anthropic);
        assert_eq!(disc.models[1].context_length, Some(500_000), "the API's own number wins");
        let haiku = &disc.models[2];
        assert_eq!(haiku.context_length, Some(200_000), "the documented window when the API names none");
        assert_eq!(haiku.thinking.presets.join(","), "off,low,medium,high");
        assert_eq!(llm.profile().map(|p| p.presets.len()), Some(4), "the active model's presets are adopted");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen[0].starts_with("GET /v1/models?limit=1000 ") && seen[0].to_ascii_lowercase().contains("\r\nx-api-key: k\r\n"), "{}", seen[0]);
    }

    #[tokio::test]
    async fn without_a_key_the_model_list_is_not_asked_for() {
        let llm = Client::new(Endpoint::new(ApiProtocol::Anthropic, "http://127.0.0.1:9", None), "claude-opus-5");
        assert!(llm.discover_server().await.is_none());
    }
}
