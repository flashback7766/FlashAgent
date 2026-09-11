//! Streaming parsers: SSE byte decoding, OpenAI chunk normalization,
//! and text-embedded tool-call extraction (Hermes, Mistral, bare JSON).

use serde_json::Value;

use crate::repair::repair_json;
use crate::types::{FinishReason, LlmEvent};

/// Decodes `data: ...` SSE lines from arbitrary byte chunks.
/// Yields JSON payload strings; `[DONE]` is reported as `Some("[DONE]")`.
#[derive(Default)]
pub struct SseDecoder {
    buf: Vec<u8>,
}

impl SseDecoder {
    /// Feed raw bytes; returns completed data-payloads.
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

/// Find the end of the next SSE record (blank-line terminated).
fn find_double_newline(buf: &[u8]) -> Option<(usize, usize)> {
    buf.windows(2)
        .position(|w| w == b"\n\n")
        .map(|p| (p, p + 2))
        .or_else(|| {
            buf.windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|p| (p, p + 4))
        })
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

/// Normalizes OpenAI-compatible chat chunks into [`LlmEvent`]s.
#[derive(Default)]
pub struct ChunkParser {
    tool_ids: Vec<Option<String>>,
    tool_names: Vec<Option<String>>,
}

impl ChunkParser {
    /// Parse one SSE payload (a chunk JSON or `[DONE]`).
    pub fn feed(&mut self, payload: &str) -> Vec<LlmEvent> {
        if payload.trim() == "[DONE]" {
            return vec![LlmEvent::Done(FinishReason::Stop)];
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

        if let Some(usage) = v.get("usage").filter(|u| u.is_object()) {
            let prompt = usage.get("prompt_tokens").and_then(Value::as_i64);
            let completion = usage.get("completion_tokens").and_then(Value::as_i64);
            let cached = usage
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .or_else(|| usage.get("cached_tokens"))
                .and_then(Value::as_i64);
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
            let Some(delta) = choice.get("delta") else {
                continue;
            };
            if let Some(reasoning) = delta
                .get("reasoning_content")
                .or_else(|| delta.get("reasoning"))
                .and_then(Value::as_str)
            {
                events.push(LlmEvent::ReasoningDelta(reasoning.to_string()));
            }
            if let Some(text) = delta.get("content").and_then(Value::as_str) {
                events.push(LlmEvent::TextDelta(text.to_string()));
            }
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                    while self.tool_ids.len() <= index {
                        self.tool_ids.push(None);
                        self.tool_names.push(None);
                    }
                    let id = call
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| self.tool_ids[index].clone());
                    self.tool_ids[index] = Some(id.clone().unwrap_or_default());
                    let name = call
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| self.tool_names[index].clone());
                    self.tool_names[index] = name.clone();
                    // Some shims (Ollama, older vLLM) send the arguments as a
                    // JSON object instead of the spec's string.
                    let args_delta = match call.get("function").and_then(|f| f.get("arguments")) {
                        Some(Value::String(s)) => s.clone(),
                        Some(Value::Null) | None => String::new(),
                        Some(other) => other.to_string(),
                    };
                    events.push(LlmEvent::ToolCallDelta { index, id, name, args_delta });
                }
            }
            if let Some(finish) = choice.get("finish_reason").and_then(Value::as_str) {
                let reason = match finish {
                    "tool_calls" => FinishReason::ToolUse,
                    "length" => FinishReason::Length,
                    _ => FinishReason::Stop,
                };
                events.push(LlmEvent::Done(reason));
            }
        }
        events
    }
}

/// Output of [`TextToolScanner::feed`].
#[derive(Debug, Clone, PartialEq)]
pub enum ScannerEvent {
    /// Plain text to show (tool-call markup removed).
    Text(String),
    /// A complete, repaired tool call extracted from text.
    ToolCall {
        /// Tool name.
        name: String,
        /// Repaired JSON arguments.
        args_json: String,
        /// The markup exactly as the model wrote it, so a caller that rejects
        /// the call (unknown tool name) can put the text back verbatim.
        raw: String,
    },
}

/// Extracts tool calls embedded in plain text, for models without native
/// tool-calling. Recognized formats:
/// - Hermes/Qwen: `<tool_call>{"name":...,"arguments":{...}}</tool_call>`
/// - Mistral: `[TOOL_CALLS][{...}]`
/// - Bare JSON object whose first key is `"name"` (typical degenerate output)
#[derive(Default)]
pub struct TextToolScanner {
    buf: String,
}

const START_HERMES: &str = "<tool_call>";
const END_HERMES: &str = "</tool_call>";
const START_MISTRAL: &str = "[TOOL_CALLS]";

impl TextToolScanner {
    /// Feed a text delta; returns flushed text/tool-call events.
    pub fn feed(&mut self, delta: &str) -> Vec<ScannerEvent> {
        self.buf.push_str(delta);
        let mut out = Vec::new();
        loop {
            // Flush plain text sitting before the next marker first.
            if let Some(text) = self.take_leading_text() {
                out.push(ScannerEvent::Text(text));
                continue;
            }
            let found = self
                .find_hermes()
                .or_else(|| self.find_mistral())
                .or_else(|| if self.inside_code_fence() { None } else { self.find_bare() });
            if let Some((body, consumed)) = found {
                let raw: String = self.buf.drain(..consumed).collect();
                out.push(self.call_or_text(&body, raw));
                continue;
            }
            break;
        }
        // Flush everything except bytes held back as a potential partial marker.
        let keep = self.hold_back();
        let flush_len = self.buf.len() - keep;
        if flush_len > 0 {
            let text = self.buf.drain(..flush_len).collect::<String>();
            out.push(ScannerEvent::Text(text));
        }
        out
    }

    /// Flush whatever is still held back once the stream has ended. An
    /// unterminated block gets one repair attempt (models often stop right
    /// before `</tool_call>`); anything unparseable comes back as plain text,
    /// never silently dropped.
    pub fn finish(&mut self) -> Vec<ScannerEvent> {
        let bare = !self.inside_code_fence() && self.find_bare_start() == Some(self.buf.len() - self.buf.trim_start().len());
        let rest = std::mem::take(&mut self.buf);
        if rest.trim().is_empty() {
            return if rest.is_empty() { Vec::new() } else { vec![ScannerEvent::Text(rest)] };
        }
        let trimmed = rest.trim_start();
        let body = trimmed
            .strip_prefix(START_HERMES)
            .or_else(|| trimmed.strip_prefix(START_MISTRAL))
            .or_else(|| bare.then_some(trimmed));
        match body {
            Some(b) => vec![self.call_or_text(strip_partial_close(b), rest.clone())],
            None => vec![ScannerEvent::Text(rest)],
        }
    }

    fn call_or_text(&self, body: &str, raw: String) -> ScannerEvent {
        match self.parse_body(body) {
            Some((name, args_json)) => ScannerEvent::ToolCall { name, args_json, raw },
            None => ScannerEvent::Text(raw),
        }
    }

    /// Emit buffered text that precedes a recognized marker; None when the
    /// buffer starts with a marker (or no complete marker is present).
    fn take_leading_text(&mut self) -> Option<String> {
        let markers = [START_HERMES, START_MISTRAL, "```tool_calls", "```tool_call"];
        let mut best: Option<usize> = None;
        for m in markers {
            if let Some(p) = self.buf.find(m) {
                best = Some(match best {
                    Some(b) => b.min(p),
                    None => p,
                });
            }
        }
        // A bare-JSON candidate also cuts the text (whitespace-tolerant).
        if best.is_none() && !self.inside_code_fence() {
            if let Some(p) = self.find_bare_start() {
                best = Some(p);
            }
        }
        let cut = best?;
        if cut == 0 {
            return None;
        }
        Some(self.buf.drain(..cut).collect())
    }

    /// Offset of a bare-JSON candidate (`{` + whitespace + `"name"` or tool key), either
    /// at buffer start or right after a newline. None when absent.
    fn find_bare_start(&self) -> Option<usize> {
        let candidates = [0usize, self.buf.find('\n').map(|p| p + 1).unwrap_or(usize::MAX)];
        for start in candidates {
            if start >= self.buf.len() {
                continue;
            }
            let rest = &self.buf[start..];
            let trimmed = rest.trim_start();
            let lead = rest.len() - trimmed.len();
            if let Some(after) = trimmed.strip_prefix('{') {
                if is_tool_call_start(after) {
                    return Some(start + lead);
                }
            }
        }
        None
    }

    /// True while an odd number of ``` fences has been seen (we never treat
    /// fenced content as tool markup — it is display code).
    fn inside_code_fence(&self) -> bool {
        let mut fences = 0usize;
        let mut rest = self.buf.as_str();
        while let Some(p) = rest.find("\n```") {
            fences += 1;
            rest = &rest[p + 1..];
        }
        if self.buf.starts_with("```") {
            fences += 1;
        }
        fences % 2 == 1
    }

    /// Bytes we must not flush yet. Entire buffer is held when a tool-call
    /// block is open (marker seen, close not yet arrived) — its content is
    /// markup, not display text. Otherwise only a partial-marker suffix.
    fn hold_back(&self) -> usize {
        let hermes_open = self.buf.find(START_HERMES).is_some() && self.buf.find(END_HERMES).is_none();
        let mistral_open = self.buf.find(START_MISTRAL).is_some() && self.find_mistral().is_none();
        let bare_open = !self.inside_code_fence() && self.find_bare_start().is_some() && self.find_bare().is_none();
        if hermes_open || mistral_open || bare_open || self.starts_with_block() {
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
        hold.min(self.buf.len())
    }

    fn starts_with_block(&self) -> bool {
        self.buf.trim_start().starts_with(START_HERMES)
    }

    /// Hermes tag pair; returns (body, consumed).
    fn find_hermes(&self) -> Option<(String, usize)> {
        let start = self.buf.find(START_HERMES)? + START_HERMES.len();
        let end = self.buf[start..].find(END_HERMES)? + start;
        let body = self.buf[start..end].trim().to_string();
        Some((body, end + END_HERMES.len()))
    }

    /// Mistral prefix + trailing JSON array/object.
    fn find_mistral(&self) -> Option<(String, usize)> {
        let start = self.buf.find(START_MISTRAL)? + START_MISTRAL.len();
        let rest = &self.buf[start..];
        let arr_start = rest.find('[')?;
        // Consume to the matching close bracket of the first element object.
        let mut depth = 0i32;
        let mut in_str = false;
        let mut esc = false;
        for (i, c) in rest[arr_start..].char_indices() {
            match c {
                '"' if !esc => in_str = !in_str,
                '\\' if in_str => esc = !esc,
                _ => esc = false,
            }
            if in_str {
                continue;
            }
            match c {
                '[' | '{' => depth += 1,
                ']' | '}' => {
                    depth -= 1;
                    if depth == 0 {
                        let body = rest[arr_start..=arr_start + i].to_string();
                        let consumed = start + arr_start + i + 1;
                        return Some((body, consumed));
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Bare JSON object starting at a line beginning with `{` + tool indicator.
    fn find_bare(&self) -> Option<(String, usize)> {
        let start = self.find_bare_start()?;
        let trimmed = &self.buf[start..];
        let mut depth = 0i32;
        let mut in_str = false;
        let mut esc = false;
        for (i, c) in trimmed.char_indices() {
            match c {
                '"' if !esc => in_str = !in_str,
                '\\' if in_str => esc = !esc,
                _ => esc = false,
            }
            if in_str {
                continue;
            }
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        let body = trimmed[..=i].to_string();
                        let consumed = start + i + 1;
                        return Some((body, consumed));
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Parse a JSON body into (name, repaired args). Handles `name`+`arguments`,
    /// `name`+`parameters`, OpenAI `function` wrapper, single-key tool wrappers
    /// like `{"ask_user": {...}}`, and flat properties directly on the object.
    fn parse_body(&self, body: &str) -> Option<(String, String)> {
        let cleaned = body
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .trim();
        let repaired = repair_json(cleaned)?;
        let mut v: Value = serde_json::from_str(&repaired).ok()?;
        if let Some(arr) = v.as_array() {
            if let Some(first) = arr.first() {
                v = first.clone();
            }
        }
        let mut obj = v.as_object()?.clone();

        // Format A: OpenAI function wrapper {"function": {"name": ..., "arguments": ...}}
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

        // Format B: Standard {"name": "...", "arguments": ...}
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
                    // If obj still has keys (e.g. {"question": "...", "options": [...]}),
                    // these remaining keys ARE the tool arguments!
                    obj.remove("id");
                    obj.remove("type");
                    if !obj.is_empty() {
                        serde_json::to_string(&obj).unwrap_or_else(|_| "{}".to_string())
                    } else {
                        "{}".to_string()
                    }
                }
            };
            return Some((name, args_json));
        }

        // Format C: Single-key object where key is tool name: {"ask_user": {"question": "..."}}
        if obj.len() == 1 {
            let (k, val) = obj.iter().next()?;
            let name = k.clone();
            let args_json = match val {
                Value::Object(o) => serde_json::to_string(o).unwrap_or_default(),
                Value::String(s) => repair_json(s).unwrap_or_else(|| s.clone()),
                other => other.to_string(),
            };
            return Some((name, args_json));
        }

        None
    }
}

/// Drop a closing `</tool_call>` (complete or cut off mid-marker) from the end
/// of an unterminated block body.
fn strip_partial_close(body: &str) -> &str {
    let body = body.trim_end();
    if let Some(pos) = body.rfind("</") {
        if END_HERMES.starts_with(&body[pos..]) {
            return body[..pos].trim_end();
        }
    }
    body
}

fn is_tool_call_start(s: &str) -> bool {
    let trimmed = s.trim_start();
    let candidates = [
        "\"name\"", "'name'", "\"tool\"", "'tool'", "\"function\"", "'function'",
        "\"ask_user\"", "'ask_user'", "\"run_shell\"", "'run_shell'",
        "\"read_file\"", "'read_file'", "\"write_file\"", "'write_file'",
        "\"edit_file\"", "'edit_file'", "\"patch_file\"", "'patch_file'",
        "\"list_dir\"", "'list_dir'", "\"glob\"", "'glob'", "\"grep\"", "'grep'",
    ];
    candidates.iter().any(|c| trimmed.starts_with(c))
}

#[cfg(test)]
mod tests {
    use super::*;

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

