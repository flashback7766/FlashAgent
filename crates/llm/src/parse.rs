use serde_json::Value;

use crate::repair::repair_json;
use crate::types::{FinishReason, LlmEvent};

/// Server-sent events as the standard reads them: a line ends at CRLF, LF or
/// a lone CR (servers mix them), an empty line ends the event, its `data:`
/// lines are joined with newlines, and comments (`: OPENROUTER PROCESSING`)
/// and other fields (`event:`, `id:`) are skipped. `[DONE]` is returned as
/// the payload `"[DONE]"`. Each byte is looked at once, however the body is
/// cut into chunks.
#[derive(Default)]
pub struct SseDecoder {
    /// An unfinished line.
    line: Vec<u8>,
    /// The `data` of the event being read; bytes, so a character cut
    /// between two chunks is whole again before it is decoded.
    data: Vec<u8>,
    has_data: bool,
    /// The last chunk ended in CR: an LF opening the next one is the same break.
    after_cr: bool,
    seen_line: bool,
}

impl SseDecoder {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = chunk;
        if std::mem::take(&mut self.after_cr) {
            if let Some(stripped) = rest.strip_prefix(b"\n") {
                rest = stripped;
            } else if rest.is_empty() {
                self.after_cr = true;
            }
        }
        while let Some(end) = rest.iter().position(|&b| b == b'\n' || b == b'\r') {
            if self.line.is_empty() {
                self.take_line(&rest[..end], &mut out);
            } else {
                let mut line = std::mem::take(&mut self.line);
                line.extend_from_slice(&rest[..end]);
                self.take_line(&line, &mut out);
                line.clear();
                self.line = line;
            }
            let cr = rest[end] == b'\r';
            rest = &rest[end + 1..];
            if cr {
                match rest.first() {
                    Some(b'\n') => rest = &rest[1..],
                    None => self.after_cr = true,
                    Some(_) => {}
                }
            }
        }
        self.line.extend_from_slice(rest);
        out
    }

    /// The body has ended: an event the server did not close with an empty
    /// line is still delivered.
    pub fn finish(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        let line = std::mem::take(&mut self.line);
        if !line.is_empty() {
            self.take_line(&line, &mut out);
        }
        self.dispatch(&mut out);
        out
    }

    fn take_line(&mut self, line: &[u8], out: &mut Vec<String>) {
        let line = if std::mem::replace(&mut self.seen_line, true) { line } else { line.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(line) };
        if line.is_empty() {
            return self.dispatch(out);
        }
        let (field, value) = match line.iter().position(|&b| b == b':') {
            Some(0) => return,
            Some(colon) => (&line[..colon], &line[colon + 1..]),
            None => (line, &line[line.len()..]),
        };
        if field == b"data" {
            if self.has_data {
                self.data.push(b'\n');
            }
            self.data.extend_from_slice(value.strip_prefix(b" ").unwrap_or(value));
            self.has_data = true;
        }
    }

    fn dispatch(&mut self, out: &mut Vec<String>) {
        if !std::mem::take(&mut self.has_data) {
            return;
        }
        let data = std::mem::take(&mut self.data);
        if !data.is_empty() {
            out.push(String::from_utf8(data).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()));
        }
    }
}

/// OpenAI stream chunks into events, whatever the server copied them from.
/// Strings are moved out of the parsed chunk, not copied.
#[derive(Default)]
pub struct ChunkParser {
    tool_ids: Vec<Option<String>>,
    tool_names: Vec<Option<String>>,
    thought: ThoughtTags,
    /// Readers stop at the first `Done`, so there is only one: a finish
    /// reason followed by `[DONE]` must not end the stream twice.
    done: bool,
}

const THOUGHT_OPEN: &str = "<thought>";
const THOUGHT_CLOSE: &str = "</thought>";

/// Gemini writes thought summaries into the text between `<thought>` and
/// `</thought>`, besides or instead of marking them as thought. The tags are
/// never shown, and what is between them is reasoning. Text marked as thought
/// is reasoning whole, but its tags still count: a model may open one there
/// and close it at the start of its answer. Such a tag only ends at its
/// close, and the unmarked text before that is still the answer, so a close
/// that never comes cannot swallow it.
#[derive(Default)]
pub(crate) struct ThoughtTags {
    state: Tagged,
    /// A tag cut between two pieces, waiting for the rest, and whether it
    /// came in marked text.
    carry: String,
    carry_marked: bool,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum Tagged {
    #[default]
    Answer,
    Thought,
    /// Opened in text marked as thought.
    OpenedInThought,
}

impl ThoughtTags {
    pub(crate) fn split(&mut self, text: String, marked: bool, events: &mut Vec<LlmEvent>) {
        if text.is_empty() {
            return;
        }
        if self.state == Tagged::Answer && self.carry.is_empty() && !text.contains('<') {
            events.push(if marked { LlmEvent::ReasoningDelta(text) } else { LlmEvent::TextDelta(text) });
            return;
        }
        if marked != self.carry_marked {
            self.flush(events);
        }
        let joined = std::mem::take(&mut self.carry) + &text;
        let mut rest = joined.as_str();
        loop {
            let tag = if self.state == Tagged::Answer { THOUGHT_OPEN } else { THOUGHT_CLOSE };
            match rest.find(tag) {
                Some(at) => {
                    self.emit(&rest[..at], marked, events);
                    self.state = match (self.state, marked) {
                        (Tagged::Answer, true) => Tagged::OpenedInThought,
                        (Tagged::Answer, false) => Tagged::Thought,
                        _ => Tagged::Answer,
                    };
                    rest = &rest[at + tag.len()..];
                }
                None => {
                    // Hold what may be the start of the tag.
                    let keep = (1..tag.len()).rev().find(|&n| rest.ends_with(&tag[..n])).unwrap_or(0);
                    let cut = rest.len() - keep;
                    self.emit(&rest[..cut], marked, events);
                    self.carry = rest[cut..].to_string();
                    self.carry_marked = marked;
                    return;
                }
            }
        }
    }

    /// Before whatever comes next, and at the end: a held `<` was just text.
    pub(crate) fn flush(&mut self, events: &mut Vec<LlmEvent>) {
        let carry = std::mem::take(&mut self.carry);
        self.emit(&carry, self.carry_marked, events);
    }

    fn emit(&self, piece: &str, marked: bool, events: &mut Vec<LlmEvent>) {
        if piece.is_empty() {
            return;
        }
        events.push(if marked || self.state == Tagged::Thought {
            LlmEvent::ReasoningDelta(piece.to_string())
        } else {
            LlmEvent::TextDelta(piece.to_string())
        });
    }
}

impl ChunkParser {
    /// At the end, a held `<` was just text.
    fn flush_carry(&mut self, events: &mut Vec<LlmEvent>) {
        self.thought.flush(events);
    }

    pub fn feed(&mut self, payload: &str) -> Vec<LlmEvent> {
        let mut events = Vec::new();
        if payload.trim() == "[DONE]" {
            self.end(&mut events);
        } else if let Ok(v) = serde_json::from_str::<Value>(payload) {
            self.feed_value(v, &mut events);
        }
        events
    }

    /// The body ended without `[DONE]` or a finish reason: a held `<` was text.
    pub fn finish(&mut self) -> Vec<LlmEvent> {
        let mut events = Vec::new();
        self.flush_carry(&mut events);
        events
    }

    /// `[DONE]`.
    pub(crate) fn end(&mut self, events: &mut Vec<LlmEvent>) {
        self.flush_carry(events);
        self.finish_with(FinishReason::Stop, events);
    }

    fn finish_with(&mut self, reason: FinishReason, events: &mut Vec<LlmEvent>) {
        if std::mem::replace(&mut self.done, true) {
            return;
        }
        // Some servers say `stop` after calling tools.
        let reason = if reason == FinishReason::Stop && self.tool_names.iter().any(Option::is_some) { FinishReason::ToolUse } else { reason };
        events.push(LlmEvent::Done(reason));
    }

    /// One chunk, already parsed.
    pub(crate) fn feed_value(&mut self, mut v: Value, events: &mut Vec<LlmEvent>) {
        let mtp = v.get("stats").or_else(|| v.get("speculative_stats")).and_then(|s| {
            let total = s.get("total_draft_tokens_count")
                .or_else(|| s.get("total_draft_tokens"))
                .or_else(|| s.get("draft_tokens"))
                .and_then(Value::as_u64)?;
            let accepted = s.get("accepted_draft_tokens_count")
                .or_else(|| s.get("accepted_draft_tokens"))
                .or_else(|| s.get("accepted_tokens"))
                .and_then(Value::as_u64)?;
            let rejected = s.get("rejected_draft_tokens_count")
                .or_else(|| s.get("rejected_draft_tokens"))
                .or_else(|| s.get("rejected_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(total.saturating_sub(accepted));
            Some(crate::types::MtpStats {
                total_draft_tokens: total,
                accepted_draft_tokens: accepted,
                rejected_draft_tokens: rejected,
            })
        });

        let timings = v.get("timings").filter(|t| t.is_object()).and_then(|t| {
            let cache_n = t.get("cache_n").and_then(Value::as_i64)?;
            let prompt_n = t.get("prompt_n").and_then(Value::as_i64).unwrap_or(0);
            Some((prompt_n + cache_n, cache_n))
        });

        // Groq puts the usage of its last chunk under `x_groq`.
        let usage = v.get("usage").filter(|u| u.is_object()).or_else(|| v.pointer("/x_groq/usage").filter(|u| u.is_object()));
        if let Some(usage) = usage {
            let mut prompt = usage.get("prompt_tokens").and_then(Value::as_i64);
            let completion = usage.get("completion_tokens").and_then(Value::as_i64);
            // Cached tokens by provider: OpenAI, OpenRouter, Gemini, xAI, vLLM use
            // `prompt_tokens_details.cached_tokens`; DeepSeek `prompt_cache_hit_tokens`;
            // Anthropic-style `cache_read_input_tokens`; llama.cpp `timings`.
            let cache_read = usage.get("cache_read_input_tokens").and_then(Value::as_i64);
            let cached = usage
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .or_else(|| usage.get("cached_tokens"))
                .or_else(|| usage.get("prompt_cache_hit_tokens"))
                .and_then(Value::as_i64)
                .or(cache_read)
                .or(timings.map(|(_, cached)| cached));
            // Anthropic counts cache reads apart from input tokens.
            if let (Some(p), Some(read)) = (prompt, cache_read) {
                if read > p {
                    prompt = Some(p + read);
                }
            }
            let mtp = mtp.or_else(|| {
                let details = usage.get("completion_tokens_details")?;
                let accepted = details.get("accepted_prediction_tokens").and_then(Value::as_u64)?;
                let rejected = details.get("rejected_prediction_tokens").and_then(Value::as_u64).unwrap_or(0);
                Some(crate::types::MtpStats {
                    total_draft_tokens: accepted + rejected,
                    accepted_draft_tokens: accepted,
                    rejected_draft_tokens: rejected,
                })
            });
            events.push(LlmEvent::Usage(crate::types::Usage {
                prompt,
                completion,
                cached,
                mtp,
            }));
        } else if let Some((prompt, cached)) = timings {
            events.push(LlmEvent::Usage(crate::types::Usage {
                prompt: Some(prompt),
                completion: None,
                cached: Some(cached),
                mtp,
            }));
        } else if let Some(m) = mtp {
            events.push(LlmEvent::Usage(crate::types::Usage {
                prompt: None,
                completion: None,
                cached: None,
                mtp: Some(m),
            }));
        }

        let Some(choices) = v.get_mut("choices").and_then(Value::as_array_mut) else {
            return;
        };
        for choice in choices {
            // A reply that was not streamed has a `message` where a chunk has its `delta`.
            let key = if choice.get("delta").is_some() { "delta" } else { "message" };
            if let Some(delta) = choice.get_mut(key) {
                self.delta(delta, events);
            }
            // A last chunk may carry only the finish reason, and `length` must not be
            // lost. Some gateways send `""` on every chunk until the real one.
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str).filter(|r| !r.is_empty()) {
                self.flush_carry(events);
                self.finish_with(finish_reason(reason), events);
            }
        }
    }

    fn delta(&mut self, delta: &mut Value, events: &mut Vec<LlmEvent>) {
        // `"reasoning_content": null` beside a real `"reasoning"` is common, and
        // OpenRouter repeats its `reasoning` in `reasoning_details`.
        let reasoning = ["reasoning_content", "reasoning"]
            .iter()
            .find_map(|k| match delta.get_mut(*k) {
                Some(Value::String(s)) if !s.is_empty() => Some(std::mem::take(s)),
                _ => None,
            })
            .or_else(|| delta.get("reasoning_details").map(reasoning_details_text).filter(|t| !t.is_empty()));
        if let Some(reasoning) = reasoning {
            events.push(LlmEvent::ReasoningDelta(reasoning));
        }
        let marked_thought = delta.pointer("/extra_content/google/thought").and_then(Value::as_bool) == Some(true);
        match delta.get_mut("content").map(Value::take) {
            Some(Value::String(text)) => self.content(text, marked_thought, events),
            // Mistral's reasoning models send parts: `thinking` ones, then `text`.
            Some(Value::Array(parts)) => {
                for mut part in parts {
                    match part.get_mut("thinking").map(Value::take) {
                        Some(thinking) => {
                            let text = match thinking {
                                Value::String(s) => s,
                                other => reasoning_details_text(&other),
                            };
                            if !text.is_empty() {
                                events.push(LlmEvent::ReasoningDelta(text));
                            }
                        }
                        None => {
                            if let Some(Value::String(text)) = part.get_mut("text").map(Value::take) {
                                self.content(text, marked_thought, events);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        if let Some(Value::Array(calls)) = delta.get_mut("tool_calls").map(Value::take) {
            for call in calls {
                self.tool_call(call, events);
            }
        }
        // The API before tools: one call, no id.
        if let Some(function) = delta.get_mut("function_call").map(Value::take).filter(Value::is_object) {
            let mut call = serde_json::Map::new();
            call.insert("index".into(), Value::from(0));
            call.insert("function".into(), function);
            self.tool_call(Value::Object(call), events);
        }
    }

    fn content(&mut self, text: String, marked_thought: bool, events: &mut Vec<LlmEvent>) {
        self.thought.split(text, marked_thought, events);
    }

    fn tool_call(&mut self, mut call: Value, events: &mut Vec<LlmEvent>) {
        let non_empty = |v: Option<Value>| match v {
            Some(Value::String(s)) if !s.is_empty() => Some(s),
            _ => None,
        };
        let fresh_id = non_empty(call.get_mut("id").map(Value::take));
        let mut function = call.get_mut("function").map(Value::take).unwrap_or(Value::Null);
        let fresh_name = non_empty(function.get_mut("name").map(Value::take));
        let index = match call.get("index").and_then(Value::as_u64) {
            // A garbage index must not grow the table without bound.
            Some(i) => (i as usize).min(self.tool_ids.len() + 64),
            // Gemini leaves `index` out. A call begins with its name: under a new id,
            // or without one after a call that already has its name.
            None => match self.tool_ids.len().checked_sub(1) {
                None => 0,
                Some(last) => {
                    let same_id = fresh_id.is_some() && self.tool_ids[last] == fresh_id;
                    let begins = fresh_name.is_some() && !same_id && (fresh_id.is_some() || self.tool_names[last].is_some());
                    last + usize::from(begins)
                }
            },
        };
        while self.tool_ids.len() <= index {
            self.tool_ids.push(None);
            self.tool_names.push(None);
        }
        // An empty id or name in a later chunk is absent, not a rename.
        let id = fresh_id.or_else(|| self.tool_ids[index].clone().filter(|id| !id.is_empty()));
        self.tool_ids[index] = Some(id.clone().unwrap_or_default());
        let name = fresh_name.or_else(|| self.tool_names[index].clone());
        self.tool_names[index].clone_from(&name);
        // Ollama and older vLLM send arguments as an object, not a string.
        let args_delta = match function.get_mut("arguments").map(Value::take) {
            Some(Value::String(s)) => s,
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        };
        events.push(LlmEvent::ToolCallDelta { index, id, name, args_delta });
    }
}

/// OpenRouter's `reasoning_details` and Mistral's thinking parts: a list of
/// pieces with `text` (or `summary`); encrypted ones have neither.
fn reasoning_details_text(details: &Value) -> String {
    let Some(items) = details.as_array() else { return String::new() };
    let mut text = String::new();
    for item in items {
        if let Some(piece) = item.get("text").or_else(|| item.get("summary")).and_then(Value::as_str) {
            text.push_str(piece);
        }
    }
    text
}

fn finish_reason(finish: &str) -> FinishReason {
    match finish {
        "tool_calls" | "function_call" | "tool_use" => FinishReason::ToolUse,
        "length" | "max_tokens" | "max_output_tokens" | "MAX_TOKENS" => FinishReason::Length,
        _ => FinishReason::Stop,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ScannerEvent {
    Text(String),
    ToolCall {
        name: String,
        /// Repaired JSON.
        args_json: String,
        /// The markup as written, so a rejected call can be put back as text.
        raw: String,
    },
}

/// Tool calls embedded in text, for models without native tool calling:
/// Hermes `<tool_call>`, Mistral `[TOOL_CALLS]`, or bare JSON with a `"name"`.
/// Anything inside a ``` fence is display text; fence state is kept across
/// deltas.
#[derive(Default)]
pub struct TextToolScanner {
    buf: String,
    fence_open: bool,
    mid_line: bool,
    /// A call block the buffer starts with and that has not closed yet.
    open: Option<OpenBlock>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Hermes,
    Mistral,
    CallFence,
    Bare,
}

/// How far an open call block has been read, so each delta is checked for
/// its close from there. Read from its start every time, a long call (a
/// file written through `write_file`) cost time quadratic in its length.
struct OpenBlock {
    kind: BlockKind,
    scanned: usize,
    balance: Balance,
}

impl OpenBlock {
    fn new(kind: BlockKind, buf: &str) -> Self {
        let mut block = OpenBlock { kind, scanned: 0, balance: Balance::default() };
        let from = match kind {
            BlockKind::Mistral => START_MISTRAL.len(),
            BlockKind::CallFence => START_CALL_FENCE.len(),
            BlockKind::Hermes | BlockKind::Bare => 0,
        };
        block.scanned = from.min(buf.len());
        block.closes(buf);
        block
    }

    /// Whether what `buf` gained since the last look closes the block.
    fn closes(&mut self, buf: &str) -> bool {
        let closed = match self.kind {
            BlockKind::Hermes => {
                let mut from = self.scanned.saturating_sub(END_HERMES.len() - 1).max(START_HERMES.len()).min(buf.len());
                while !buf.is_char_boundary(from) {
                    from -= 1;
                }
                buf[from..].contains(END_HERMES)
            }
            BlockKind::CallFence => {
                let mut from = self.scanned.saturating_sub(CALL_FENCE_END.len() - 1).max(START_CALL_FENCE.len()).min(buf.len());
                while !buf.is_char_boundary(from) {
                    from -= 1;
                }
                self.balance.advance(&buf[self.scanned..]) || buf[from..].contains(CALL_FENCE_END)
            }
            BlockKind::Mistral | BlockKind::Bare => self.balance.advance(&buf[self.scanned..]),
        };
        self.scanned = buf.len();
        closed
    }
}

/// `balanced_len` a piece at a time, from the first bracket on.
#[derive(Default)]
struct Balance {
    started: bool,
    depth: i32,
    in_str: bool,
    esc: bool,
    closed: bool,
}

impl Balance {
    /// True once the value that opened first has closed, and from then on.
    fn advance(&mut self, text: &str) -> bool {
        if self.closed {
            return true;
        }
        for c in text.chars() {
            if !self.started {
                if !matches!(c, '{' | '[') {
                    continue;
                }
                self.started = true;
            }
            if self.in_str {
                match c {
                    '\\' if !self.esc => self.esc = true,
                    '"' if !self.esc => self.in_str = false,
                    _ => self.esc = false,
                }
                continue;
            }
            match c {
                '"' => self.in_str = true,
                '{' | '[' => self.depth += 1,
                '}' | ']' => {
                    self.depth -= 1;
                    if self.depth == 0 {
                        self.closed = true;
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }
}

const START_HERMES: &str = "<tool_call>";
const END_HERMES: &str = "</tool_call>";
const START_MISTRAL: &str = "[TOOL_CALLS]";
/// A fence tagged as a call around its JSON: what Gemma writes when told to
/// use `<tool_call>`. Unlike a ```json fence, which may hold an example, the
/// tag says it is meant to run.
const START_CALL_FENCE: &str = "```tool_call";
/// Its closing fence, on a line of its own. A JSON string holds no raw line
/// break, so a fence inside the call's content is never taken for it.
const CALL_FENCE_END: &str = "\n```";

impl TextToolScanner {
    pub fn feed(&mut self, delta: &str) -> Vec<ScannerEvent> {
        self.buf.push_str(delta);
        if let Some(open) = self.open.as_mut() {
            if !open.closes(&self.buf) {
                return Vec::new();
            }
            self.open = None;
        }
        let mut out = Vec::new();
        loop {
            if let Some(text) = self.take_leading_text() {
                self.emit_text(text, &mut out);
                continue;
            }
            let found = self.find_hermes().or_else(|| self.find_mistral()).or_else(|| self.find_call_fence()).or_else(|| self.find_bare());
            if let Some((body, consumed)) = found {
                let raw: String = self.buf.drain(..consumed).collect();
                for ev in self.calls_or_text(&body, raw) {
                    match ev {
                        ScannerEvent::Text(t) => self.emit_text(t, &mut out),
                        call => out.push(call),
                    }
                }
                continue;
            }
            break;
        }
        if let Some(kind) = self.open_block() {
            self.open = Some(OpenBlock::new(kind, &self.buf));
            return out;
        }
        let keep = self.hold_back();
        let flush_len = self.buf.len() - keep;
        if flush_len > 0 {
            let text = self.buf.drain(..flush_len).collect::<String>();
            self.emit_text(text, &mut out);
        }
        out
    }

    /// A block missing only `</tool_call>` still counts if its JSON is complete;
    /// a body cut off mid-arguments would be "completed" by repair into something
    /// the model never wrote. Everything else is returned as text.
    pub fn finish(&mut self) -> Vec<ScannerEvent> {
        self.open = None;
        let rest = std::mem::take(&mut self.buf);
        if rest.is_empty() {
            return Vec::new();
        }
        let trimmed = rest.trim_start();
        let body = (!self.fence_open)
            .then(|| {
                trimmed
                    .strip_prefix(START_HERMES)
                    .or_else(|| trimmed.strip_prefix(START_MISTRAL))
                    .map(strip_partial_close)
                    .or_else(|| trimmed.strip_prefix(START_CALL_FENCE).map(|b| b.trim_end().trim_end_matches('`')))
            })
            .flatten()
            .filter(|b| is_complete_json(b));
        let mut out = Vec::new();
        match body {
            Some(b) => {
                for ev in self.calls_or_text(b, rest.clone()) {
                    match ev {
                        ScannerEvent::Text(t) => self.emit_text(t, &mut out),
                        call => out.push(call),
                    }
                }
            }
            None => self.emit_text(rest, &mut out),
        }
        out
    }

    fn emit_text(&mut self, text: String, out: &mut Vec<ScannerEvent>) {
        if text.is_empty() {
            return;
        }
        self.fence_open = self.fence_open_after(&text);
        self.mid_line = !text.ends_with('\n');
        out.push(ScannerEvent::Text(text));
    }

    /// Only a fence at the start of a line counts.
    fn fence_open_after(&self, text: &str) -> bool {
        let bytes = text.as_bytes();
        let mut open = self.fence_open;
        let mut line_start = !self.mid_line;
        for i in 0..bytes.len() {
            if line_start && bytes[i..].starts_with(b"```") {
                open = !open;
            }
            line_start = bytes[i] == b'\n';
        }
        open
    }

    fn in_fence_at(&self, pos: usize) -> bool {
        self.fence_open_after(&self.buf[..pos])
    }

    fn find_outside_fence(&self, pat: &str) -> Option<usize> {
        self.buf.match_indices(pat).map(|(p, _)| p).find(|&p| !self.in_fence_at(p))
    }

    fn calls_or_text(&self, body: &str, raw: String) -> Vec<ScannerEvent> {
        match parse_calls(body) {
            Some(calls) if calls.len() == 1 => {
                let (name, args_json) = calls.into_iter().next().unwrap_or_default();
                vec![ScannerEvent::ToolCall { name, args_json, raw }]
            }
            Some(calls) if !calls.is_empty() => calls
                .into_iter()
                .map(|(name, args_json)| {
                    let raw = serde_json::json!({ "name": name, "arguments": serde_json::from_str::<Value>(&args_json).unwrap_or(Value::Null) }).to_string();
                    ScannerEvent::ToolCall { name, args_json, raw }
                })
                .collect(),
            _ => vec![ScannerEvent::Text(raw)],
        }
    }

    fn take_leading_text(&mut self) -> Option<String> {
        let markers = [START_HERMES, START_MISTRAL, START_CALL_FENCE];
        let cut = markers
            .iter()
            .filter_map(|m| self.find_outside_fence(m))
            .chain(self.find_bare_start())
            .min()?;
        if cut == 0 {
            return None;
        }
        Some(self.buf.drain(..cut).collect())
    }

    fn find_bare_start(&self) -> Option<usize> {
        let line_starts = std::iter::once(0)
            .filter(|_| !self.mid_line)
            .chain(self.buf.match_indices('\n').map(|(p, _)| p + 1));
        for start in line_starts {
            if start >= self.buf.len() {
                continue;
            }
            let rest = &self.buf[start..];
            let trimmed = rest.trim_start_matches([' ', '\t']);
            let at = start + (rest.len() - trimmed.len());
            if (trimmed.strip_prefix('{').is_some_and(is_tool_call_start)
                || trimmed.strip_prefix('[').is_some_and(is_tool_call_start))
                && !self.in_fence_at(at)
            {
                return Some(at);
            }
            if let Some(after_fence) = trimmed.strip_prefix("```") {
                let after_lang = after_fence.strip_prefix("json").unwrap_or(after_fence);
                if let Some(after_nl) = after_lang.strip_prefix("\r\n").or_else(|| after_lang.strip_prefix('\n')) {
                    let inner = after_nl.trim_start_matches([' ', '\t']);
                    if inner.strip_prefix('{').is_some_and(is_tool_call_start)
                        || inner.strip_prefix('[').is_some_and(is_tool_call_start)
                    {
                        return Some(at);
                    }
                }
            }
        }
        None
    }

    /// A call block begun and not closed. Text before the first marker has
    /// been let go by now, so the block starts the buffer.
    /// Its kind is what starts the buffer: a marker further on may sit inside
    /// the block, in a file the call writes.
    fn open_block(&self) -> Option<BlockKind> {
        let hermes_open = self.find_outside_fence(START_HERMES).is_some() && self.find_hermes().is_none();
        let mistral_open = self.find_outside_fence(START_MISTRAL).is_some() && self.find_mistral().is_none();
        let fence_open = self.find_outside_fence(START_CALL_FENCE).is_some() && self.find_call_fence().is_none();
        let bare_open = self.find_bare_start().is_some() && self.find_bare().is_none();
        if !(hermes_open || mistral_open || fence_open || bare_open) {
            return None;
        }
        Some(if self.buf.starts_with(START_HERMES) {
            BlockKind::Hermes
        } else if self.buf.starts_with(START_MISTRAL) {
            BlockKind::Mistral
        } else if self.buf.starts_with(START_CALL_FENCE) {
            BlockKind::CallFence
        } else {
            BlockKind::Bare
        })
    }

    /// While a tool-call block is open the whole buffer is held (see
    /// `open_block`); otherwise only a suffix that may start a marker.
    fn hold_back(&self) -> usize {
        let markers = [START_HERMES, END_HERMES, START_MISTRAL, START_CALL_FENCE, "\"name\"", "\"ask_user\""];
        let mut hold = 0usize;
        for m in markers {
            for skip in 1..m.len() {
                if self.buf.ends_with(&m[..skip]) {
                    hold = hold.max(skip);
                }
            }
        }
        // Every trailing backtick: a fence is only seen when all three arrive
        // together, and one sent on its own would flip nothing.
        hold = hold.max(self.buf.len() - self.buf.trim_end_matches('`').len());
        // What is being written may still become a bare call: `{"na`, a `{` on a
        // line of its own with the key on the next (pretty-printed), or a
        // ```json fence whose call has not come yet. Once its start went out,
        // the rest was no longer at a line start and the call was never seen.
        let line_starts = std::iter::once(0)
            .filter(|_| !self.mid_line)
            .chain(self.buf.match_indices('\n').map(|(p, _)| p + 1));
        for start in line_starts {
            if start >= self.buf.len() || self.in_fence_at(start) {
                continue;
            }
            if could_become_call(&self.buf[start..]) {
                hold = hold.max(self.buf.len() - start);
                break;
            }
        }
        hold.min(self.buf.len())
    }

    fn find_hermes(&self) -> Option<(String, usize)> {
        let start = self.find_outside_fence(START_HERMES)?;
        if start != 0 {
            return None;
        }
        let body_start = START_HERMES.len();
        let end = self.buf[body_start..].find(END_HERMES)? + body_start;
        let body = self.buf[body_start..end].trim().to_string();
        Some((body, end + END_HERMES.len()))
    }

    fn find_mistral(&self) -> Option<(String, usize)> {
        let start = self.find_outside_fence(START_MISTRAL)?;
        if start != 0 {
            return None;
        }
        let rest = &self.buf[START_MISTRAL.len()..];
        let open = rest.find(['[', '{'])?;
        let len = balanced_len(&rest[open..])?;
        Some((rest[open..open + len].to_string(), START_MISTRAL.len() + open + len))
    }

    /// The call once the fence has closed, or once its JSON has and the text
    /// after it shows no fence is coming: a closing fence let out as text
    /// would read as one opening, and hide every later call. Between closed
    /// fences the body is repaired like a Hermes block's.
    fn find_call_fence(&self) -> Option<(String, usize)> {
        let start = self.find_outside_fence(START_CALL_FENCE)?;
        if start != 0 {
            return None;
        }
        let rest = &self.buf[START_CALL_FENCE.len()..];
        if let Some(end) = rest.find(CALL_FENCE_END) {
            return Some((rest[..end].trim().to_string(), START_CALL_FENCE.len() + end + CALL_FENCE_END.len()));
        }
        let open = rest.find(['[', '{'])?;
        let len = balanced_len(&rest[open..])?;
        let after = &rest[open + len..];
        let next = after.trim_start();
        let close = if next.starts_with("```") {
            after.len() - next.len() + 3
        } else if next.is_empty() || "```".starts_with(next) {
            return None;
        } else {
            0
        };
        Some((rest[open..open + len].to_string(), START_CALL_FENCE.len() + open + len + close))
    }

    fn find_bare(&self) -> Option<(String, usize)> {
        let start = self.find_bare_start()?;
        if start != 0 {
            return None;
        }
        let rest = &self.buf;
        let trimmed = rest.trim_start_matches([' ', '\t']);
        let offset = rest.len() - trimmed.len();
        if let Some(after_fence) = trimmed.strip_prefix("```") {
            let after_lang = after_fence.strip_prefix("json").unwrap_or(after_fence);
            let header_len = after_lang.as_ptr() as usize - trimmed.as_ptr() as usize;
            let (after_nl, nl_len) = if let Some(s) = after_lang.strip_prefix("\r\n") {
                (s, 2)
            } else {
                let s = after_lang.strip_prefix('\n')?;
                (s, 1)
            };
            let inner = after_nl.trim_start_matches([' ', '\t']);
            let inner_offset = after_nl.len() - inner.len();
            let json_len = balanced_len(inner)?;
            let after_json = &inner[json_len..];
            let close_fence_len = if let Some(p) = after_json.find("```") {
                p + 3
            } else {
                0
            };
            let consumed = offset + header_len + nl_len + inner_offset + json_len + close_fence_len;
            return Some((inner[..json_len].to_string(), consumed));
        }
        let len = balanced_len(trimmed)?;
        Some((trimmed[..len].to_string(), offset + len))
    }
}

/// String-aware; None while the value is still open.
fn balanced_len(text: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for (i, c) in text.char_indices() {
        if in_str {
            match c {
                '\\' if !esc => esc = true,
                '"' if !esc => in_str = false,
                _ => esc = false,
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' | '[' => depth += 1,
            '}' | ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + c.len_utf8());
                }
            }
            _ => {}
        }
    }
    None
}

/// Cosmetic repair (quotes, trailing commas) is allowed.
fn is_complete_json(body: &str) -> bool {
    let body = body.trim();
    balanced_len(body).is_some_and(|len| body[len..].trim().is_empty())
}

/// An array fails as a whole if any element is not a call.
fn parse_calls(body: &str) -> Option<Vec<(String, String)>> {
    let raw = body.trim();
    let unquoted = if let Some(rest) = raw.strip_prefix("```") {
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        let rest = rest.strip_prefix('\n').unwrap_or(rest);
        rest.strip_suffix("```").unwrap_or(rest).trim()
    } else {
        raw
    };
    let repaired = repair_json(unquoted)?;
    let v: Value = serde_json::from_str(&repaired).ok()?;
    match v {
        Value::Array(ref items)
            if items.len() == 2 && items[0].is_string() && items[1].is_object() =>
        {
            let name = items[0].as_str()?.to_string();
            let args_json = serde_json::to_string(&items[1]).ok()?;
            Some(vec![(name, args_json)])
        }
        Value::Array(items) => items.into_iter().map(parse_call_object).collect(),
        other => parse_call_object(other).map(|c| vec![c]),
    }
}

/// Accepts `name`+`arguments`/`parameters`, the OpenAI `function` wrapper,
/// `{"<tool>": {..}}`, and flat properties on the object.
fn parse_call_object(v: Value) -> Option<(String, String)> {
    let mut obj = v.as_object()?.clone();

    if let Some(func) = obj.get("function").and_then(Value::as_object) {
        let name = func.get("name").and_then(Value::as_str)?.to_string();
        let args = func.get("arguments").or_else(|| func.get("parameters"));
        let args_json = match args {
            Some(Value::String(s)) => repair_json(s).unwrap_or_else(|| s.clone()),
            Some(Value::Object(o)) => serde_json::to_string(o).unwrap_or_default(),
            Some(other) => other.to_string(),
            None => "{}".to_string(),
        };
        return Some((name, args_json));
    }

    let name_opt = obj
        .remove("name")
        .or_else(|| obj.remove("tool"))
        .and_then(|v| v.as_str().map(str::to_string));

    if let Some(name) = name_opt {
        let args = obj
            .remove("arguments")
            .or_else(|| obj.remove("args"))
            .or_else(|| obj.remove("parameters"));
        let args_json = match args {
            Some(Value::String(s)) => repair_json(&s).unwrap_or(s),
            Some(Value::Object(o)) => serde_json::to_string(&o).unwrap_or_default(),
            Some(other) => other.to_string(),
            None => {
                // The remaining keys are the arguments.
                obj.remove("id");
                obj.remove("type");
                serde_json::to_string(&obj).unwrap_or_else(|_| "{}".to_string())
            }
        };
        return Some((name, args_json));
    }

    if obj.len() == 1 {
        let (k, val) = obj.iter().next()?;
        let args_json = match val {
            Value::Object(o) => serde_json::to_string(o).unwrap_or_default(),
            Value::String(s) => repair_json(s).unwrap_or_else(|| s.clone()),
            other => other.to_string(),
        };
        return Some((k.clone(), args_json));
    }

    None
}

fn strip_partial_close(body: &str) -> &str {
    let body = body.trim_end();
    if let Some(pos) = body.rfind("</") {
        if END_HERMES.starts_with(&body[pos..]) {
            return body[..pos].trim_end();
        }
    }
    body
}

/// The text from a line start is the unfinished head of a bare call, maybe
/// pretty-printed. A fence is not held for one: JSON in a fence is an
/// example, and running an example is worse than missing a call.
fn could_become_call(rest: &str) -> bool {
    let head = rest.trim_start_matches([' ', '\t']);
    head.strip_prefix('{').or_else(|| head.strip_prefix('[')).is_some_and(could_start_tool_call)
}

/// Not yet a call, but nothing written so far rules one out.
fn could_start_tool_call(s: &str) -> bool {
    let trimmed = s.trim_start();
    TOOL_CALL_KEYS.iter().any(|c| c.starts_with(trimmed) || trimmed.starts_with(c))
}

fn is_tool_call_start(s: &str) -> bool {
    let trimmed = s.trim_start();
    TOOL_CALL_KEYS.iter().any(|c| trimmed.starts_with(c))
}

const TOOL_CALL_KEYS: [&str; 24] = [
        "\"name\"", "'name'", "\"tool\"", "'tool'", "\"function\"", "'function'",
        "\"ask_user\"", "'ask_user'", "\"run_shell\"", "'run_shell'",
        "\"read_file\"", "'read_file'", "\"write_file\"", "'write_file'",
        "\"edit_file\"", "'edit_file'", "\"patch_file\"", "'patch_file'",
        "\"list_dir\"", "'list_dir'", "\"glob\"", "'glob'", "\"grep\"", "'grep'",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fence_that_arrives_in_its_own_delta_still_hides_its_example() {
        let example = r#"<tool_call>{"name":"run_shell","arguments":{"command":"rm -rf /"}}</tool_call>"#;
        assert!(scan(&["Format:\n", "```", "\n", example, "\n", "```", "\n", "Done."]).1.is_empty());
        assert!(scan(&["Format:\n", "``", "`\n", example, "\n`", "``\n"]).1.is_empty());
        // A closing fence on its own does not leave the fence open.
        let real = r#"<tool_call>{"name":"read_file","arguments":{"path":"a.rs"}}</tool_call>"#;
        assert_eq!(scan(&["```\n", "code\n", "```", "\n", real]).1, vec!["read_file"]);
    }

    #[test]
    fn a_bare_call_split_across_deltas_is_still_found() {
        let rest = r#"": "read_file", "arguments": {"path": "a.rs"}}"#;
        assert_eq!(scan(&["Reading.\n", "{\"", "name", rest]).1, vec!["read_file"]);
        assert_eq!(scan(&["Reading.\n", "{", "\"name", rest]).1, vec!["read_file"]);
        // Pretty-printed and split at every token.
        let pretty = ["Reading.\n", "{\n", "  \"name\"", ": \"read_file\",\n", "  \"arguments\": {\"path\": \"a.rs\"}\n", "}"];
        assert_eq!(scan(&pretty).1, vec!["read_file"]);
        // Prose that only starts like one comes out as text.
        let (text, calls) = scan(&["Here:\n", "{", "\"id\": 1}\n"]);
        assert!(calls.is_empty());
        assert_eq!(text, "Here:\n{\"id\": 1}\n");
    }

    #[test]
    fn a_thought_tag_opened_in_a_marked_chunk_and_closed_in_the_answer_is_never_shown() {
        let mut p = ChunkParser::default();
        let marked = |t: &str| serde_json::json!({ "choices": [ { "delta": { "content": t, "extra_content": { "google": { "thought": true } } } } ] }).to_string();
        let plain = |t: &str| serde_json::json!({ "choices": [ { "delta": { "content": t } } ] }).to_string();
        let mut events = p.feed(&marked("<thought>Acknowledge"));
        events.extend(p.feed(&plain("</thought>Hello!")));
        events.extend(p.feed("[DONE]"));
        let reasoning: String = events.iter().filter_map(|e| match e { LlmEvent::ReasoningDelta(t) => Some(t.as_str()), _ => None }).collect();
        let text: String = events.iter().filter_map(|e| match e { LlmEvent::TextDelta(t) => Some(t.as_str()), _ => None }).collect();
        assert_eq!((reasoning.as_str(), text.as_str()), ("Acknowledge", "Hello!"));
    }

    #[test]
    fn gemini_thoughts_in_the_content_are_reasoning_even_split_across_chunks() {
        let mut p = ChunkParser::default();
        let chunk = |t: &str| serde_json::json!({ "choices": [ { "delta": { "content": t } } ] }).to_string();
        let mut events = Vec::new();
        for piece in ["<thou", "ght>Planning the ", "answer</tho", "ught>Hello", " <b>there</b>"] {
            events.extend(p.feed(&chunk(piece)));
        }
        let reasoning: String = events.iter().filter_map(|e| match e { LlmEvent::ReasoningDelta(t) => Some(t.as_str()), _ => None }).collect();
        let text: String = events.iter().filter_map(|e| match e { LlmEvent::TextDelta(t) => Some(t.as_str()), _ => None }).collect();
        assert_eq!(reasoning, "Planning the answer");
        assert_eq!(text, "Hello <b>there</b>");
        let mut p = ChunkParser::default();
        let mut ends = p.feed(&chunk("a < b and b <"));
        ends.extend(p.feed("[DONE]"));
        let text: String = ends.iter().filter_map(|e| match e { LlmEvent::TextDelta(t) => Some(t.as_str()), _ => None }).collect();
        assert_eq!(text, "a < b and b <");
        let mut p = ChunkParser::default();
        let marked = r#"{"choices":[{"delta":{"content":"weighing","extra_content":{"google":{"thought":true}}}}]}"#;
        assert_eq!(p.feed(marked), vec![LlmEvent::ReasoningDelta("weighing".into())]);
    }

    #[test]
    fn stream_details_some_servers_send_are_read() {
        let mut p = ChunkParser::default();
        assert_eq!(p.feed(r#"{"choices":[{"index":0,"finish_reason":"length"}]}"#), vec![LlmEvent::Done(FinishReason::Length)]);
        let ev = p.feed(r#"{"choices":[{"delta":{"reasoning_content":null,"reasoning":"x"}}]}"#);
        assert_eq!(ev, vec![LlmEvent::ReasoningDelta("x".into())]);
        let mut p = ChunkParser::default();
        p.feed(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read_file","arguments":"{"}}]}}]}"#);
        let ev = p.feed(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"","function":{"name":"","arguments":"}"}}]}}]}"#);
        assert!(matches!(&ev[0], LlmEvent::ToolCallDelta { id: Some(id), name: Some(name), .. } if id == "c1" && name == "read_file"), "{ev:?}");
        let mut d = SseDecoder::default();
        assert_eq!(d.feed(b"data: A\r\n\r\ndata: B\r\n\r\n: ping\n\n"), vec!["A", "B"]);
    }

    #[test]
    fn sse_decoder_handles_split_chunks() {
        let mut d = SseDecoder::default();
        assert!(d.feed(b"data: {\"a\"").is_empty());
        let got = d.feed(b":1}\n\ndata: [DONE]\n\n");
        assert_eq!(got, vec![r#"{"a":1}"#, "[DONE]"]);
    }

    fn sse_in_pieces(pieces: &[&[u8]]) -> Vec<String> {
        let mut d = SseDecoder::default();
        let mut out = Vec::new();
        for piece in pieces {
            out.extend(d.feed(piece));
        }
        out.extend(d.finish());
        out
    }

    #[test]
    fn an_event_ends_at_an_empty_line_whatever_the_line_endings() {
        assert_eq!(sse_in_pieces(&[b"data: A\r\rdata: B\r\r"]), vec!["A", "B"], "a lone CR ends a line");
        assert_eq!(sse_in_pieces(&[b"data: A\n\r\ndata: B\r\n\n"]), vec!["A", "B"], "mixed endings");
        // A CRLF cut between two chunks is one line break, not two.
        assert_eq!(sse_in_pieces(&[b"data: A\r", b"\ndata: B\r\n\r\n"]), vec!["A\nB"]);
        assert_eq!(sse_in_pieces(&[b"data: A\r", b"", b"\n\r", b"\n"]), vec!["A"]);
    }

    #[test]
    fn comments_and_other_fields_are_skipped_and_data_lines_are_joined() {
        let body: &[u8] = b"\xEF\xBB\xBF: OPENROUTER PROCESSING\n\nevent: message\nid: 7\nretry: 100\ndata: {\"a\":\ndata: 1}\n\ndata:{\"b\":2}\n\ndata\n\n:\n\n";
        assert_eq!(sse_in_pieces(&[body]), vec!["{\"a\":\n1}", "{\"b\":2}"]);
        // Only one space after the colon belongs to the syntax.
        assert_eq!(sse_in_pieces(&[b"data:  x\n\n"]), vec![" x"]);
    }

    #[test]
    fn a_character_cut_between_chunks_arrives_whole() {
        let body = "data: {\"t\":\"héllo — 世界\"}\n\n".as_bytes();
        for cut in 1..body.len() {
            assert_eq!(sse_in_pieces(&[&body[..cut], &body[cut..]]), vec!["{\"t\":\"héllo — 世界\"}"], "cut at {cut}");
        }
    }

    #[test]
    fn the_last_event_is_delivered_even_without_a_closing_empty_line() {
        let mut d = SseDecoder::default();
        assert_eq!(d.feed(b"data: {\"a\":1}\n\ndata: [DONE]"), vec![r#"{"a":1}"#]);
        assert_eq!(d.finish(), vec!["[DONE]"]);
        assert!(d.finish().is_empty(), "delivered once");
        assert_eq!(sse_in_pieces(&[b"data: x\n"]), vec!["x"]);
    }

    #[test]
    fn a_long_stream_fed_a_byte_at_a_time_is_read_in_linear_time() {
        // Searching the whole buffer on every chunk made this quadratic: minutes, not milliseconds.
        let text = "x".repeat(400_000);
        let body = format!("data: {text}\n\n");
        let started = std::time::Instant::now();
        let mut d = SseDecoder::default();
        let mut out = Vec::new();
        for byte in body.as_bytes() {
            out.extend(d.feed(std::slice::from_ref(byte)));
        }
        assert_eq!(out, vec![text]);
        let many: String = (0..20_000).map(|i| format!("data: {i}\n\n")).collect();
        assert_eq!(SseDecoder::default().feed(many.as_bytes()).len(), 20_000);
        assert!(started.elapsed() < std::time::Duration::from_secs(10), "{:?}", started.elapsed());
    }

    fn events_of(payloads: &[&str]) -> Vec<LlmEvent> {
        let mut p = ChunkParser::default();
        let mut events: Vec<LlmEvent> = payloads.iter().flat_map(|payload| p.feed(payload)).collect();
        events.extend(p.finish());
        events
    }

    fn reasoning_of(events: &[LlmEvent]) -> String {
        events.iter().filter_map(|e| match e { LlmEvent::ReasoningDelta(t) => Some(t.as_str()), _ => None }).collect()
    }

    fn text_of(events: &[LlmEvent]) -> String {
        events.iter().filter_map(|e| match e { LlmEvent::TextDelta(t) => Some(t.as_str()), _ => None }).collect()
    }

    fn dones(events: &[LlmEvent]) -> Vec<FinishReason> {
        events.iter().filter_map(|e| match e { LlmEvent::Done(r) => Some(*r), _ => None }).collect()
    }

    #[test]
    fn reasoning_is_read_under_every_name_servers_give_it_and_only_once() {
        let events = events_of(&[
            r#"{"choices":[{"delta":{"reasoning_content":"a"}}]}"#,
            r#"{"choices":[{"delta":{"reasoning_content":"","reasoning":"b"}}]}"#,
            // OpenRouter: the same text in `reasoning` and in `reasoning_details`.
            r#"{"choices":[{"delta":{"reasoning":"c","reasoning_details":[{"type":"reasoning.text","text":"c"}]}}]}"#,
            r#"{"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.summary","summary":"d"},{"type":"reasoning.encrypted","data":"zz"}]}}]}"#,
            // Mistral's reasoning models: content parts.
            r#"{"choices":[{"delta":{"content":[{"type":"thinking","thinking":[{"type":"text","text":"e"}]}]}}]}"#,
            r#"{"choices":[{"delta":{"content":[{"type":"text","text":"Answer"}]}}]}"#,
        ]);
        assert_eq!(reasoning_of(&events), "abcde");
        assert_eq!(text_of(&events), "Answer");
    }

    #[test]
    fn empty_pieces_are_not_events() {
        let events = events_of(&[r#"{"choices":[{"delta":{"role":"assistant","content":"","reasoning_content":""}}]}"#, r#"{"choices":[{"delta":{"content":null}}]}"#]);
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn an_empty_finish_reason_does_not_end_the_stream() {
        // Some gateways send `"finish_reason": ""` on every chunk; readers stop at the first Done.
        let events = events_of(&[
            r#"{"choices":[{"delta":{"content":"a"},"finish_reason":""}]}"#,
            r#"{"choices":[{"delta":{"content":"b"},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"length"}]}"#,
        ]);
        assert_eq!(events, vec![LlmEvent::TextDelta("a".into()), LlmEvent::TextDelta("b".into()), LlmEvent::Done(FinishReason::Length)]);
    }

    #[test]
    fn the_stream_ends_once_and_usage_after_the_finish_reason_still_arrives() {
        let events = events_of(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"read_file","arguments":"{}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2}}"#,
            "[DONE]",
        ]);
        assert_eq!(dones(&events), vec![FinishReason::ToolUse]);
        assert!(matches!(events.last(), Some(LlmEvent::Usage(u)) if u.prompt == Some(10)), "{events:?}");
    }

    #[test]
    fn every_way_of_saying_why_the_answer_ended_is_understood() {
        let reason = |r: &str| dones(&events_of(&[&format!(r#"{{"choices":[{{"delta":{{}},"finish_reason":"{r}"}}]}}"#)]));
        assert_eq!(reason("stop"), vec![FinishReason::Stop]);
        assert_eq!(reason("length"), vec![FinishReason::Length]);
        assert_eq!(reason("max_tokens"), vec![FinishReason::Length]);
        assert_eq!(reason("content_filter"), vec![FinishReason::Stop]);
        assert_eq!(reason("tool_calls"), vec![FinishReason::ToolUse]);
        assert_eq!(reason("function_call"), vec![FinishReason::ToolUse]);
        // Tool calls, then `stop` (Ollama's /v1 and some vLLM versions) or no reason at all.
        let call = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"grep","arguments":"{}"}}]}}]}"#;
        assert_eq!(dones(&events_of(&[call, r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#])), vec![FinishReason::ToolUse]);
        assert_eq!(dones(&events_of(&[call, "[DONE]"])), vec![FinishReason::ToolUse]);
    }

    fn calls_of(events: &[LlmEvent]) -> Vec<(usize, Option<String>, Option<String>, String)> {
        let mut calls: Vec<(usize, Option<String>, Option<String>, String)> = Vec::new();
        for e in events {
            if let LlmEvent::ToolCallDelta { index, id, name, args_delta } = e {
                match calls.iter_mut().find(|c| c.0 == *index) {
                    Some(c) => {
                        c.1 = id.clone().or(c.1.take());
                        c.2 = name.clone().or(c.2.take());
                        c.3.push_str(args_delta);
                    }
                    None => calls.push((*index, id.clone(), name.clone(), args_delta.clone())),
                }
            }
        }
        calls
    }

    #[test]
    fn a_legacy_function_call_is_one_tool_call() {
        let events = events_of(&[
            r#"{"choices":[{"delta":{"function_call":{"name":"read_file","arguments":""}}}]}"#,
            r#"{"choices":[{"delta":{"function_call":{"arguments":"{\"path\":"}}}]}"#,
            r#"{"choices":[{"delta":{"function_call":{"arguments":"\"a.rs\"}"}}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"function_call"}]}"#,
        ]);
        assert_eq!(calls_of(&events), vec![(0, None, Some("read_file".into()), r#"{"path":"a.rs"}"#.into())]);
        assert_eq!(dones(&events), vec![FinishReason::ToolUse]);
    }

    #[test]
    fn calls_without_an_index_or_an_id_are_told_apart_by_their_names() {
        let events = events_of(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"function":{"name":"read_file","arguments":"{\"path\":\"a\"}"}},{"function":{"name":"grep","arguments":"{}"}}]}}]}"#,
        ]);
        let calls = calls_of(&events);
        assert_eq!(calls.iter().map(|c| (c.0, c.2.clone().unwrap())).collect::<Vec<_>>(), vec![(0, "read_file".into()), (1, "grep".into())]);
        // One call in fragments, the id repeated or changed, the name only first: still one call.
        let events = events_of(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"x1","function":{"name":"read_file","arguments":"{\"pa"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"x1","function":{"arguments":"th\":"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"x2","function":{"arguments":"\"a\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"function":{"arguments":""}}]}}]}"#,
        ]);
        let calls = calls_of(&events);
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0].3, r#"{"path":"a"}"#);
    }

    #[test]
    fn a_garbage_index_does_not_grow_the_call_table_without_bound() {
        let events = events_of(&[r#"{"choices":[{"delta":{"tool_calls":[{"index":18446744073709551615,"id":"c","function":{"name":"grep","arguments":"{}"}}]}}]}"#]);
        assert!(matches!(events.first(), Some(LlmEvent::ToolCallDelta { index, .. }) if *index <= 64));
    }

    #[test]
    fn groq_usage_in_its_own_field_is_read() {
        let u = usage_of(r#"{"choices":[{"delta":{},"finish_reason":"stop"}],"x_groq":{"id":"req_1","usage":{"prompt_tokens":120,"completion_tokens":8}}}"#);
        assert_eq!((u.prompt, u.completion), (Some(120), Some(8)));
    }

    #[test]
    fn a_held_angle_bracket_is_text_when_the_body_ends_without_done() {
        let events = events_of(&[r#"{"choices":[{"delta":{"content":"a <"}}]}"#]);
        assert_eq!(text_of(&events), "a <");
        assert!(dones(&events).is_empty(), "the pump adds the Done");
    }

    #[test]
    fn chunk_parser_text_and_reasoning() {
        let mut p = ChunkParser::default();
        let ev = p.feed(
            r#"{"choices":[{"delta":{"content":"Hi","reasoning_content":"thinking"}}]}"#,
        );
        assert!(ev.contains(&LlmEvent::TextDelta("Hi".into())));
        assert!(ev.contains(&LlmEvent::ReasoningDelta("thinking".into())));
    }

    #[test]
    fn chunk_parser_tool_call_fragments_merge() {
        let mut p = ChunkParser::default();
        let e1 = p.feed(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"shell","arguments":"{\"cm"}}]}}]}"#,
        );
        let e2 = p.feed(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"_done"}}]}}],"usage":{"prompt_tokens":9,"completion_tokens":4}}"#,
        );
        assert!(matches!(
            e1.first(),
            Some(LlmEvent::ToolCallDelta { id: Some(id), name: Some(n), .. })
                if id == "c1" && n == "shell"
        ));
        assert!(matches!(
            e1.last(),
            Some(LlmEvent::ToolCallDelta { args_delta, .. }) if args_delta == "{\"cm"
        ));
        assert!(e2.iter().any(|e| matches!(e, LlmEvent::Usage(_))));
        assert!(e2.iter().any(|e| matches!(
            e,
            LlmEvent::ToolCallDelta { args_delta, .. } if args_delta == "_done"
        )));
    }

    fn usage_of(payload: &str) -> crate::types::Usage {
        let events = ChunkParser::default().feed(payload);
        events
            .into_iter()
            .find_map(|e| match e {
                LlmEvent::Usage(u) => Some(u),
                _ => None,
            })
            .expect("a usage event")
    }

    #[test]
    fn the_cache_figure_is_read_whatever_the_provider_calls_it() {
        let u = usage_of(r#"{"choices":[],"usage":{"prompt_tokens":1000,"completion_tokens":5,"prompt_tokens_details":{"cached_tokens":900}}}"#);
        assert_eq!((u.prompt, u.cached), (Some(1000), Some(900)));
        let u = usage_of(r#"{"choices":[],"usage":{"prompt_tokens":1000,"completion_tokens":5,"prompt_cache_hit_tokens":750,"prompt_cache_miss_tokens":250}}"#);
        assert_eq!((u.prompt, u.cached), (Some(1000), Some(750)));
        let u = usage_of(r#"{"choices":[],"usage":{"prompt_tokens":40,"completion_tokens":5,"cache_read_input_tokens":960}}"#);
        assert_eq!((u.prompt, u.cached), (Some(1000), Some(960)));
        let u = usage_of(r#"{"choices":[],"timings":{"prompt_n":15,"cache_n":3500}}"#);
        assert_eq!((u.prompt, u.cached), (Some(3515), Some(3500)));
        let u = usage_of(r#"{"choices":[],"usage":{"prompt_tokens":3515,"completion_tokens":5},"timings":{"prompt_n":15,"cache_n":3500}}"#);
        assert_eq!((u.prompt, u.cached), (Some(3515), Some(3500)));
        let u = usage_of(r#"{"choices":[],"usage":{"prompt_tokens":2913,"completion_tokens":5}}"#);
        assert_eq!(u.cached, None);
    }

    #[test]
    fn chunk_parser_accepts_object_arguments() {
        let mut p = ChunkParser::default();
        let ev = p.feed(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read_file","arguments":{"path":"a.rs"}}}]}}]}"#);
        assert!(matches!(
            ev.first(),
            Some(LlmEvent::ToolCallDelta { args_delta, .. }) if args_delta == r#"{"path":"a.rs"}"#
        ));
    }

    #[test]
    fn scanner_finish_recovers_unterminated_block_and_keeps_plain_text() {
        let mut s = TextToolScanner::default();
        let mut out = s.feed("Let me look. <tool_call>{\"name\":\"read_file\",\"arguments\":{\"path\":\"a.rs\"}}</tool_");
        out.extend(s.finish());
        assert!(out.iter().any(|e| matches!(e, ScannerEvent::ToolCall { name, .. } if name == "read_file")));
        assert_eq!(out.first(), Some(&ScannerEvent::Text("Let me look. ".into())));

        let mut s = TextToolScanner::default();
        let mut out = s.feed("<tool_call>not json at all");
        out.extend(s.finish());
        assert_eq!(out, vec![ScannerEvent::Text("<tool_call>not json at all".into())]);
    }

    fn scan(chunks: &[&str]) -> (String, Vec<String>) {
        let mut s = TextToolScanner::default();
        let mut events = Vec::new();
        for c in chunks {
            events.extend(s.feed(c));
        }
        events.extend(s.finish());
        let mut text = String::new();
        let mut calls = Vec::new();
        for e in events {
            match e {
                ScannerEvent::Text(t) => text.push_str(&t),
                ScannerEvent::ToolCall { name, .. } => calls.push(name),
            }
        }
        (text, calls)
    }

    /// `text` cut into pieces of `n` characters.
    fn pieces(text: &str, n: usize) -> Vec<String> {
        text.chars().collect::<Vec<_>>().chunks(n).map(|c| c.iter().collect()).collect()
    }

    #[test]
    fn a_call_read_a_piece_at_a_time_is_the_call_read_whole() {
        let file = "a <tool_call> in the text, [TOOL_CALLS] too, and a } or ] in a string";
        let inputs = [
            format!("Writing it.\n<tool_call>{{\"name\":\"write_file\",\"arguments\":{{\"path\":\"a.md\",\"content\":\"{file}\"}}}}</tool_call>\nDone."),
            format!("[TOOL_CALLS][{{\"name\":\"write_file\",\"arguments\":{{\"content\":\"{file}\"}}}}] after"),
            format!("Now.\n{{\"name\": \"write_file\", \"arguments\": {{\"content\": \"{file} ü\"}}}}\nDone."),
            format!("Now.\n```tool_call\n{{\"name\": \"write_file\", \"arguments\": {{\"content\": \"{file}\"}}}}\n```\nDone."),
        ];
        for input in &inputs {
            let whole = scan(&[input.as_str()]);
            assert_eq!(whole.1.len(), 1, "{input}: {whole:?}");
            for n in [1, 7] {
                let cut = pieces(input, n);
                let refs: Vec<&str> = cut.iter().map(String::as_str).collect();
                assert_eq!(scan(&refs), whole, "{input} in pieces of {n}");
            }
        }
    }

    #[test]
    fn a_fence_tagged_as_a_call_is_one_and_leaves_no_fence_open() {
        let fenced = "```tool_call\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"a.rs\"}}\n```";
        let then = "<tool_call>{\"name\":\"read_file\",\"arguments\":{\"path\":\"b.rs\"}}</tool_call>";
        let input = format!("Reading both.\n{fenced}\n{then}");
        for n in [1, 5, input.len()] {
            let cut = pieces(&input, n);
            let refs: Vec<&str> = cut.iter().map(String::as_str).collect();
            let (text, calls) = scan(&refs);
            assert_eq!(calls, ["read_file", "read_file"], "the call after it was hidden: {text:?}");
            assert!(!text.contains("```"), "{text:?}");
        }
        // Between closed fences a brace the model forgot is put back, as in a Hermes block.
        let (_, calls) = scan(&pieces("```tool_call\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"a\"}\n```\nok", 3).iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(calls, ["read_file"]);
        // The stream may end on the JSON, before the fence closes.
        let (_, calls) = scan(&["```tool_call\n{\"name\": \"read_file\", \"arguments\": {}}"]);
        assert_eq!(calls, ["read_file"]);
        // A ```json fence may be an example and stays text.
        let (_, calls) = scan(&["Example:\n```json\n", "{\"name\": \"read_file\", \"arguments\": {}}", "\n```"]);
        assert!(calls.is_empty());
    }

    #[test]
    fn fenced_examples_are_never_calls_even_when_streamed_in_pieces() {
        let example = "{\"name\": \"write_file\", \"arguments\": {\"path\": \"README.md\", \"content\": \"\"}}";
        let (text, calls) = scan(&["Example:\n```json\n", example, "\n```\nDone."]);
        assert!(calls.is_empty(), "fenced example executed: {calls:?}");
        assert!(text.contains(example), "fenced example vanished from the answer: {text}");

        let hermes = "<tool_call>{\"name\":\"run_shell\",\"arguments\":{\"command\":\"rm -rf /\"}}</tool_call>";
        let (text, calls) = scan(&["Format:\n``", "`\n", hermes, "\n```\n"]);
        assert!(calls.is_empty(), "fenced Hermes example executed: {calls:?}");
        assert!(text.contains(hermes));

        let (_, calls) = scan(&["```\ncode\n```\n", "<tool_call>{\"name\":\"read_file\",\"arguments\":{}}</tool_call>"]);
        assert_eq!(calls, vec!["read_file".to_string()]);
    }

    #[test]
    fn truncated_blocks_are_not_completed_by_repair() {
        let (text, calls) = scan(&["<tool_call>{\"name\":\"write_file\",\"arguments\":{\"path\":\"src/main.rs\",\"content\":\"fn main() {\\n let x = compute("]);
        assert!(calls.is_empty());
        assert!(text.contains("compute("));
        let (_, calls) = scan(&["\n{\"name\": \"read_file\""]);
        assert!(calls.is_empty());
    }

    #[test]
    fn mistral_block_yields_every_call() {
        let (_, calls) = scan(&[r#"[TOOL_CALLS][{"name":"read_file","arguments":{"path":"a"}},{"name":"read_file","arguments":{"path":"b"}}]"#]);
        assert_eq!(calls, vec!["read_file".to_string(), "read_file".to_string()]);
    }

    #[test]
    fn scanner_unparseable_block_returns_raw_text() {
        let mut s = TextToolScanner::default();
        let out = s.feed("<tool_call>@@@</tool_call> after");
        let text: String = out
            .iter()
            .map(|e| match e {
                ScannerEvent::Text(t) => t.clone(),
                ScannerEvent::ToolCall { raw, .. } => raw.clone(),
            })
            .collect();
        assert_eq!(text, "<tool_call>@@@</tool_call> after");
    }

    #[test]
    fn calls_without_an_index_are_told_apart_by_id() {
        let mut p = ChunkParser::default();
        let ev = p.feed(r#"{"choices":[{"delta":{"tool_calls":[
            {"id":"a","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"x\"}"}},
            {"id":"b","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"y\"}"}}
        ]}}]}"#);
        let indices: Vec<usize> = ev.iter().filter_map(|e| match e { LlmEvent::ToolCallDelta { index, .. } => Some(*index), _ => None }).collect();
        assert_eq!(indices, vec![0, 1]);
    }

    #[test]
    fn chunk_parser_finish_reason_tool_use() {
        let mut p = ChunkParser::default();
        let ev = p.feed(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#);
        assert_eq!(ev, vec![LlmEvent::Done(FinishReason::ToolUse)]);
    }

    #[test]
    fn scanner_hermes_format() {
        let mut s = TextToolScanner::default();
        let out = s.feed("Thinking... <tool_call>{\"name\":\"shell\",\"arguments\":{\"cmd\":\"ls\"}}</tool_call> done");
        assert_eq!(
            out,
            vec![
                ScannerEvent::Text("Thinking... ".into()),
                ScannerEvent::ToolCall {
                    name: "shell".into(),
                    args_json: r#"{"cmd":"ls"}"#.into(),
                    raw: r#"<tool_call>{"name":"shell","arguments":{"cmd":"ls"}}</tool_call>"#.into(),
                },
                ScannerEvent::Text(" done".into()),
            ]
        );
    }

    #[test]
    fn scanner_hermes_marker_split_across_deltas() {
        let mut s = TextToolScanner::default();
        let a = s.feed("text<tool_call>{\"na");
        let b = s.feed("me\":\"grep\",\"arguments\":{\"pat\":\"foo\"}}</tool_call>");
        let mut all = a;
        all.extend(b);
        assert!(all.iter().any(|e| matches!(e, ScannerEvent::ToolCall { name, .. } if name == "grep")));
    }

    #[test]
    fn scanner_mistral_format() {
        let mut s = TextToolScanner::default();
        let out = s.feed(r#"[TOOL_CALLS][{"name":"edit","arguments":{"path":"a.rs","content":"fn main(){}"}}]"#);
        assert!(matches!(
            out.first(),
            Some(ScannerEvent::ToolCall { name, .. }) if name == "edit"
        ));
    }

    #[test]
    fn scanner_bare_json_object() {
        let mut s = TextToolScanner::default();
        let out = s.feed("\n{\"name\": \"write_file\", \"arguments\": {\"path\": \"x.txt\", \"content\": \"hi\"}}");
        assert!(out.iter().any(|e| matches!(
            e,
            ScannerEvent::ToolCall { name, .. } if name == "write_file"
        )));
    }

    #[test]
    fn scanner_broken_args_repaired() {
        let mut s = TextToolScanner::default();
        let out = s.feed("<tool_call>{\"name\":\"shell\",\"arguments\":{\"cmd\":\"cargo test\",}}</tool_call>");
        assert!(matches!(
            out.first(),
            Some(ScannerEvent::ToolCall { args_json, .. }) if args_json.contains("cargo test")
        ));
    }

    #[test]
    fn scanner_does_not_eat_plain_code_fences() {
        let mut s = TextToolScanner::default();
        let out = s.feed("Look:\n```rust\n{\"name\": \"not_a_tool\", \"x\": 1}\n```\nend");
        let text = out.iter().map(|e| match e {
            ScannerEvent::Text(t) => t.as_str(),
            ScannerEvent::ToolCall { .. } => "TOOL",
        }).collect::<String>();
        assert!(!text.contains("TOOL"));
        assert!(text.contains("```"));
    }

    #[test]
    fn scanner_flat_arguments_handled() {
        let mut s = TextToolScanner::default();
        let out = s.feed("\n{\"name\": \"ask_user\", \"question\": \"What language?\", \"options\": [\"Python\", \"Rust\"]}");
        assert!(out.iter().any(|e| matches!(
            e,
            ScannerEvent::ToolCall { name, args_json, .. } if name == "ask_user" && args_json.contains("What language?") && args_json.contains("Python")
        )));
    }

    #[test]
    fn scanner_single_key_wrapper_handled() {
        let mut s = TextToolScanner::default();
        let out = s.feed("\n{\"ask_user\": {\"question\": \"Pick one\", \"options\": [\"A\", \"B\"]}}");
        assert!(out.iter().any(|e| matches!(
            e,
            ScannerEvent::ToolCall { name, args_json, .. } if name == "ask_user" && args_json.contains("Pick one")
        )));
    }

    #[test]
    fn scanner_python_single_quotes_repaired() {
        let mut s = TextToolScanner::default();
        let out = s.feed("\n{'name': 'ask_user', 'arguments': {'question': 'Ready?', 'multi_select': False}}");
        assert!(out.iter().any(|e| matches!(
            e,
            ScannerEvent::ToolCall { name, args_json, .. } if name == "ask_user" && args_json.contains("\"multi_select\":false")
        )));
    }

    #[test]
    fn scanner_fenced_json_array_tool_call() {
        let mut s = TextToolScanner::default();
        let delta = "Reading Cargo.toml...\n```json\n[\n  \"read_file\",\n  {\n    \"files\": [\"Cargo.toml\"]\n  }\n]\n```\ndone";
        let out = s.feed(delta);
        assert!(out.iter().any(|e| matches!(
            e,
            ScannerEvent::ToolCall { name, args_json, .. } if name == "read_file" && args_json.contains("Cargo.toml")
        )));
    }

    #[test]
    fn chunk_parser_extracts_mtp_stats() {
        let mut p = ChunkParser::default();
        let payload = r#"{"choices":[],"usage":{"prompt_tokens":17,"completion_tokens":20,"total_tokens":37},"stats":{"draft_model":"mtp.gguf","total_draft_tokens_count":16,"accepted_draft_tokens_count":13,"rejected_draft_tokens_count":3}}"#;
        let ev = p.feed(payload);
        let usage_ev = ev.iter().find_map(|e| match e {
            LlmEvent::Usage(u) => Some(u),
            _ => None,
        }).expect("found usage");

        assert_eq!(usage_ev.prompt, Some(17));
        assert_eq!(usage_ev.completion, Some(20));
        let mtp = usage_ev.mtp.expect("mtp stats present");
        assert_eq!(mtp.total_draft_tokens, 16);
        assert_eq!(mtp.accepted_draft_tokens, 13);
        assert_eq!(mtp.rejected_draft_tokens, 3);
        assert!((mtp.acceptance_rate() - 81.25).abs() < 0.01);
    }
}


