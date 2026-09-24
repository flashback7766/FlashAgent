use serde_json::Value;

use crate::repair::repair_json;
use crate::types::{FinishReason, LlmEvent};

/// `[DONE]` is returned as the payload `"[DONE]"`.
#[derive(Default)]
pub struct SseDecoder {
    buf: Vec<u8>,
}

impl SseDecoder {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = find_double_newline(&self.buf) {
            let line: Vec<u8> = self.buf.drain(..pos.1).collect();
            if let Some(payload) = extract_data(&line) {
                out.push(payload);
            }
        }
        out
    }
}

/// Whichever separator comes first: a server may mix `\r\n\r\n` records
/// with `\n\n` keep-alive comments.
fn find_double_newline(buf: &[u8]) -> Option<(usize, usize)> {
    let lf = buf.windows(2).position(|w| w == b"\n\n").map(|p| (p, p + 2));
    let crlf = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| (p, p + 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if b.0 < a.0 { b } else { a }),
        (a, b) => a.or(b),
    }
}

fn extract_data(record: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(record);
    let mut payload = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            if !payload.is_empty() {
                payload.push('\n');
            }
            payload.push_str(rest.trim_start());
        }
    }
    (!payload.is_empty()).then_some(payload)
}

#[derive(Default)]
pub struct ChunkParser {
    tool_ids: Vec<Option<String>>,
    tool_names: Vec<Option<String>>,
    /// Inside Gemini's `<thought>…</thought>` in the content.
    in_thought: bool,
    /// A tag cut between two chunks, waiting for the rest.
    tag_carry: String,
}

const THOUGHT_OPEN: &str = "<thought>";
const THOUGHT_CLOSE: &str = "</thought>";

impl ChunkParser {
    /// Gemini sends its thought summaries in the content, between `<thought>`
    /// tags (or marks a whole chunk as thought): they are reasoning, shown as
    /// such and kept out of the answer.
    fn split_thought(&mut self, text: &str, events: &mut Vec<LlmEvent>) {
        let mut rest = std::mem::take(&mut self.tag_carry) + text;
        loop {
            let tag = if self.in_thought { THOUGHT_CLOSE } else { THOUGHT_OPEN };
            match rest.find(tag) {
                Some(at) => {
                    self.emit_piece(&rest[..at], events);
                    self.in_thought = !self.in_thought;
                    rest = rest[at + tag.len()..].to_string();
                }
                None => {
                    // Hold what may be the start of the tag.
                    let keep = (1..tag.len()).rev().find(|&n| rest.ends_with(&tag[..n])).unwrap_or(0);
                    let cut = rest.len() - keep;
                    self.emit_piece(&rest[..cut], events);
                    self.tag_carry = rest[cut..].to_string();
                    return;
                }
            }
        }
    }

    /// At the end, a held `<` was just text.
    fn flush_carry(&mut self, events: &mut Vec<LlmEvent>) {
        let carry = std::mem::take(&mut self.tag_carry);
        self.emit_piece(&carry, events);
    }

    fn emit_piece(&self, piece: &str, events: &mut Vec<LlmEvent>) {
        if piece.is_empty() {
            return;
        }
        events.push(if self.in_thought {
            LlmEvent::ReasoningDelta(piece.to_string())
        } else {
            LlmEvent::TextDelta(piece.to_string())
        });
    }

    pub fn feed(&mut self, payload: &str) -> Vec<LlmEvent> {
        if payload.trim() == "[DONE]" {
            let mut events = Vec::new();
            self.flush_carry(&mut events);
            events.push(LlmEvent::Done(FinishReason::Stop));
            return events;
        }
        let Ok(v) = serde_json::from_str::<Value>(payload) else {
            return vec![];
        };
        let mut events = Vec::new();

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

        if let Some(usage) = v.get("usage").filter(|u| u.is_object()) {
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

        let Some(choices) = v.get("choices").and_then(Value::as_array) else {
            return events;
        };
        for choice in choices {
            // A last chunk may carry only the finish reason, and `length` must not be lost.
            let Some(delta) = choice.get("delta") else {
                if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str).map(finish_reason) {
                    events.push(LlmEvent::Done(reason));
                }
                continue;
            };
            // `"reasoning_content": null` beside a real `"reasoning"` is common.
            if let Some(reasoning) = ["reasoning_content", "reasoning"].iter().find_map(|k| delta.get(*k).and_then(Value::as_str)) {
                events.push(LlmEvent::ReasoningDelta(reasoning.to_string()));
            }
            if let Some(text) = delta.get("content").and_then(Value::as_str) {
                let marked_thought = delta.pointer("/extra_content/google/thought").and_then(Value::as_bool) == Some(true);
                if marked_thought {
                    events.push(LlmEvent::ReasoningDelta(text.to_string()));
                } else if self.in_thought || !self.tag_carry.is_empty() || text.contains('<') {
                    self.split_thought(text, &mut events);
                } else {
                    events.push(LlmEvent::TextDelta(text.to_string()));
                }
            }
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    // Gemini leaves `index` out: a new id is then a new call.
                    let index = match call.get("index").and_then(Value::as_u64) {
                        Some(i) => i as usize,
                        None => {
                            let id = call.get("id").and_then(Value::as_str).filter(|id| !id.is_empty());
                            let last = self.tool_ids.len().checked_sub(1);
                            match (id, last) {
                                (Some(id), Some(last)) if self.tool_ids[last].as_deref() != Some(id) => last + 1,
                                (_, Some(last)) => last,
                                (_, None) => 0,
                            }
                        }
                    };
                    while self.tool_ids.len() <= index {
                        self.tool_ids.push(None);
                        self.tool_names.push(None);
                    }
                    // An empty id or name in a later chunk is absent, not a rename.
                    let id = call
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty())
                        .map(str::to_string)
                        .or_else(|| self.tool_ids[index].clone());
                    self.tool_ids[index] = Some(id.clone().unwrap_or_default());
                    let name = call
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(Value::as_str)
                        .filter(|name| !name.is_empty())
                        .map(str::to_string)
                        .or_else(|| self.tool_names[index].clone());
                    self.tool_names[index] = name.clone();
                    // Ollama and older vLLM send arguments as an object, not a string.
                    let args_delta = match call.get("function").and_then(|f| f.get("arguments")) {
                        Some(Value::String(s)) => s.clone(),
                        Some(Value::Null) | None => String::new(),
                        Some(other) => other.to_string(),
                    };
                    events.push(LlmEvent::ToolCallDelta { index, id, name, args_delta });
                }
            }
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str).map(finish_reason) {
                self.flush_carry(&mut events);
                events.push(LlmEvent::Done(reason));
            }
        }
        events
    }
}

fn finish_reason(finish: &str) -> FinishReason {
    match finish {
        "tool_calls" | "function_call" | "tool_use" => FinishReason::ToolUse,
        "length" => FinishReason::Length,
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
}

const START_HERMES: &str = "<tool_call>";
const END_HERMES: &str = "</tool_call>";
const START_MISTRAL: &str = "[TOOL_CALLS]";

impl TextToolScanner {
    pub fn feed(&mut self, delta: &str) -> Vec<ScannerEvent> {
        self.buf.push_str(delta);
        let mut out = Vec::new();
        loop {
            if let Some(text) = self.take_leading_text() {
                self.emit_text(text, &mut out);
                continue;
            }
            let found = self.find_hermes().or_else(|| self.find_mistral()).or_else(|| self.find_bare());
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
        let rest = std::mem::take(&mut self.buf);
        if rest.is_empty() {
            return Vec::new();
        }
        let trimmed = rest.trim_start();
        let body = (!self.fence_open)
            .then(|| trimmed.strip_prefix(START_HERMES).or_else(|| trimmed.strip_prefix(START_MISTRAL)))
            .flatten()
            .map(strip_partial_close)
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
        let markers = [START_HERMES, START_MISTRAL];
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

    /// While a tool-call block is open the whole buffer is held; otherwise only
    /// a suffix that may be the start of a marker.
    fn hold_back(&self) -> usize {
        let hermes_open = self.find_outside_fence(START_HERMES).is_some() && self.find_hermes().is_none();
        let mistral_open = self.find_outside_fence(START_MISTRAL).is_some() && self.find_mistral().is_none();
        let bare_open = self.find_bare_start().is_some() && self.find_bare().is_none();
        if hermes_open || mistral_open || bare_open {
            return self.buf.len();
        }
        let markers = [START_HERMES, END_HERMES, START_MISTRAL, "\"name\"", "\"ask_user\""];
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

