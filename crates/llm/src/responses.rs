//! OpenAI Responses for models whose gateway refuses Chat Completions. The
//! conversation stays local: every request carries its own input, never a
//! server-side conversation id.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::client::{pump, Client, EventStream, Malformed, WireDecoder};
use crate::parse::SseDecoder;
use crate::types::{ChatMessage, CompletionMeasurement, CostDetails, FinishReason, LlmError, LlmEvent, Role, ThinkingEffort, ToolSpec, TurnOptions, Usage};

pub(crate) fn required_by(error: &str) -> bool {
    let lower = error.to_lowercase();
    let message = serde_json::from_str::<Value>(error).ok()
        .and_then(|v| v.pointer("/error/message").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| lower.clone()).to_lowercase();
    (message.contains("/responses") || message.contains("responses api"))
        && (message.contains("/chat/completions") || message.contains("chat completions"))
        && ["does not support", "not supported", "unsupported", "not available", "use ", "requires", "不支持"]
            .iter().any(|s| message.contains(s))
}

const RESOURCE_MISMATCH: &str = "The requested item was created under a different Azure OpenAI resource. Use the same resource that created the item to access it.";

fn resource_mismatch(error: &str) -> bool {
    let parsed = serde_json::from_str::<Value>(error).ok();
    let message = parsed.as_ref().and_then(|v| v.pointer("/error/message")).and_then(Value::as_str).unwrap_or(error).trim();
    let message = message.strip_prefix("OpenAI Responses bad request: ").unwrap_or(message);
    let Some(suffix) = message.strip_prefix(RESOURCE_MISMATCH) else { return false; };
    let suffix = suffix.trim();
    if suffix.is_empty() { return true; }
    // Gateways append a trace id to the provider's otherwise exact refusal.
    suffix.strip_prefix("[trace_id=").and_then(|s| s.strip_suffix(']')).is_some_and(|id| {
        !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    })
}

fn has_bound_reasoning(request: &Value) -> bool {
    request["input"].as_array().is_some_and(|items| items.iter().any(|item| {
        item["type"] == "reasoning" && ["id", "encrypted_content"].iter().any(|key| {
            item[*key].as_str().is_some_and(|s| !s.is_empty())
        })
    }))
}

pub(crate) async fn stream(client: &Client, messages: &[ChatMessage], tools: &[ToolSpec], options: &TurnOptions) -> Result<EventStream, LlmError> {
    let busy = client.busy();
    // Snapshot the reset generation before reading the endpoint/model: a switch
    // while this request is being built must not teach the new configuration.
    let mut learned = client.learned.read().clone();
    let url = format!("{}/responses", crate::openai::api_base(client));
    let headers = crate::openai::headers(client);
    let mut request = body(client, messages, tools, options);
    if learned.responses_visible_only { request["input"] = input(messages, true); }
    let mut retried_replay = false;
    loop {
        for key in &learned.responses_dropped {
            if let Some(map) = request.as_object_mut() { map.remove(*key); }
        }
        let response = client.post(&url, &headers, &request).await?;
        if response.status().is_success() {
            learned.merge_responses(&mut client.learned.write());
            let model = request["model"].as_str().unwrap_or_default().to_string();
            return Ok(pump(response, busy, Decoder { model, ..Default::default() }));
        }
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        if retried_replay { return Err(LlmError::Status { status, body: text }); }
        if status == 400 && !learned.responses_visible_only && resource_mismatch(&text) && has_bound_reasoning(&request) {
            // The entire reasoning item can be resource-bound, not just its id.
            // Rebuild only input: routing, effort and prior sampling repairs stay put.
            request["input"] = input(messages, true);
            learned.responses_visible_only = true;
            retried_replay = true;
            continue;
        }
        let lower = text.to_lowercase();
        // Outside that specific replay refusal, only named sampling fields can
        // give way. Tools, reasoning controls and privacy settings stay intact.
        let refused = ["temperature", "top_p"].into_iter().find(|key| {
            request.get(*key).is_some() && !learned.responses_dropped.contains(key) && lower.contains(key)
                && ["unsupported", "not supported", "not allowed", "unknown"].iter().any(|s| lower.contains(s))
        });
        if let Some(key) = refused.filter(|_| matches!(status, 400 | 422)) {
            learned.responses_dropped.push(key);
            continue;
        }
        return Err(LlmError::Status { status, body: text });
    }
}

fn visible_reasoning(message: &ChatMessage, items: &[Value]) -> String {
    if let Some(text) = message.reasoning.as_deref().filter(|s| !s.trim().is_empty()) { return text.to_string(); }
    let mut parts = Vec::new();
    for item in items.iter().filter(|item| item["type"] == "reasoning") {
        let summary = item["summary"].as_array().into_iter().flatten();
        let content = item["content"].as_array().into_iter().flatten().filter(|part| part["type"] == "reasoning_text");
        for part in summary.chain(content) {
            if let Some(text) = part["text"].as_str().filter(|s| !s.trim().is_empty()) {
                if !parts.contains(&text) { parts.push(text); }
            }
        }
    }
    parts.join("\n")
}

fn message_content(message: &ChatMessage) -> Value {
    if message.images.is_empty() { return json!(message.content); }
    let mut parts = Vec::new();
    if !message.content.is_empty() { parts.push(json!({"type": "input_text", "text": message.content})); }
    parts.extend(message.images.iter().map(|image| json!({"type": "input_image", "image_url": image})));
    Value::Array(parts)
}

fn input(messages: &[ChatMessage], visible_only: bool) -> Value {
    let mut input = Vec::new();
    for message in messages {
        match message.role {
            Role::Tool => input.push(json!({
                "type": "function_call_output", "call_id": message.tool_call_id, "output": message.content,
            })),
            Role::Assistant => {
                let items = message.replay.as_ref().filter(|r| r["protocol"] == "responses")
                    .and_then(|r| r["reasoning"].as_array()).map(Vec::as_slice).unwrap_or_default();
                input.extend(items.iter().filter(|item| !visible_only || item["type"] != "reasoning").cloned());
                if visible_only {
                    let reasoning = visible_reasoning(message, items);
                    if !reasoning.is_empty() {
                        input.push(json!({"role": "assistant", "content": format!("Earlier assistant reasoning (visible text):\n{reasoning}")}));
                    }
                }
                if !message.content.is_empty() || !message.images.is_empty() {
                    input.push(json!({"role": "assistant", "content": message_content(message)}));
                }
                for call in &message.tool_calls {
                    input.push(json!({"type": "function_call", "call_id": call.id, "name": call.name, "arguments": call.args_json}));
                }
            }
            Role::System | Role::User => {
                input.push(json!({"role": message.role.as_str(), "content": message_content(message)}));
            }
        }
    }
    Value::Array(input)
}

fn body(client: &Client, messages: &[ChatMessage], tools: &[ToolSpec], options: &TurnOptions) -> Value {
    let mut body = json!({
        "model": client.model(), "input": input(messages, false), "stream": true, "store": false,
        "include": ["reasoning.encrypted_content"],
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools.iter().map(|tool| json!({
            "type": "function", "name": tool.name, "description": tool.description,
            "parameters": serde_json::from_str::<Value>(&tool.parameters_json).unwrap_or_else(|_| json!({})),
            "strict": false,
        })).collect());
    }
    if let Some(max) = options.max_tokens { body["max_output_tokens"] = json!(max); }
    // A profile without an off rung must not promote an explicit disable.
    let disabled = match options.custom_effort.as_deref() {
        Some(effort) => effort.eq_ignore_ascii_case("off") || effort.eq_ignore_ascii_case("none"),
        None => options.thinking == ThinkingEffort::Off,
    };
    let effort = if disabled { Some("none".to_string()) } else { client.resolve_effort(messages, options) };
    if effort.is_some() || client.profile().is_some_and(|p| p.supported) {
        // A provider-chosen effort still needs a readable summary; requesting
        // one must not select an effort or turn an explicit `none` back on.
        body["reasoning"] = json!({"summary": "auto"});
        if let Some(effort) = effort {
            body["reasoning"]["effort"] = json!(if effort == "off" { "none" } else { &effort });
        }
    }
    // Local-model sampling presets are not GPT's tuned defaults. A Custom
    // preset is an explicit choice and may be adapted only on a named refusal.
    if client.sampling_is_custom() {
        if let Some(t) = options.temperature { body["temperature"] = json!(t); }
        if let Some(p) = options.top_p { body["top_p"] = json!(p); }
    }
    body
}

#[derive(Default)]
struct Call {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

#[derive(Default)]
struct Decoder {
    model: String,
    emitted_text: String,
    sse: SseDecoder,
    whole: Option<bool>,
    reply: Vec<u8>,
    calls: BTreeMap<usize, Call>,
    texts: BTreeMap<(usize, usize), String>,
    thoughts: BTreeMap<(usize, usize, bool), String>,
    reasoning: BTreeMap<usize, Value>,
    saw_reasoning: bool,
    measurement_unavailable: bool,
    terminal: bool,
    failed: bool,
    malformed: Malformed,
}

impl Decoder {
    fn call(&mut self, index: usize, item: &Value, out: &mut Vec<Result<LlmEvent, LlmError>>) {
        self.measurement_unavailable = true;
        let call = self.calls.entry(index).or_default();
        let id = item["call_id"].as_str().map(str::to_string);
        let name = item["name"].as_str().map(str::to_string);
        let args = item["arguments"].as_str().unwrap_or("");
        // Item-done repeats the complete arguments. Only the suffix not yet
        // streamed is new; concatenating both copies runs nonsense as JSON.
        let suffix = args.strip_prefix(&call.arguments).unwrap_or("").to_string();
        if !suffix.is_empty() { call.arguments.push_str(&suffix); }
        let changed = (id.is_some() && id != call.id) || (name.is_some() && name != call.name) || !suffix.is_empty();
        if let Some(id) = id { call.id = Some(id); }
        if let Some(name) = name { call.name = Some(name); }
        if changed {
            out.push(Ok(LlmEvent::ToolCallDelta { index, id: call.id.clone(), name: call.name.clone(), args_delta: suffix }));
        }
    }

    fn text(&mut self, index: usize, part: usize, text: &str, out: &mut Vec<Result<LlmEvent, LlmError>>) {
        let accumulated = self.texts.entry((index, part)).or_default();
        // A completed part can fill a streamed prefix, but cannot replace text
        // already shown. A conflicting copy is no longer a training sample.
        if let Some(suffix) = text.strip_prefix(accumulated.as_str()) {
            if !suffix.is_empty() {
                accumulated.push_str(suffix);
                self.emitted_text.push_str(suffix);
                out.push(Ok(LlmEvent::TextDelta(suffix.into())));
            }
        } else {
            self.measurement_unavailable = true;
        }
    }

    fn thought(&mut self, key: (usize, usize, bool), text: &str, delta: bool, out: &mut Vec<Result<LlmEvent, LlmError>>) {
        self.saw_reasoning = true;
        if text.is_empty() { return; }
        let accumulated = self.thoughts.entry(key).or_default();
        let suffix = if delta { text } else { text.strip_prefix(accumulated.as_str()).unwrap_or("") };
        if !suffix.is_empty() {
            accumulated.push_str(suffix);
            out.push(Ok(LlmEvent::ReasoningDelta(suffix.into())));
        }
    }

    fn completion_measurement(&self, usage: &Value) -> CompletionMeasurement {
        use CompletionMeasurement::{Text, Unavailable};
        let Some(total) = usage["output_tokens"].as_i64().filter(|n| *n >= 0) else { return Unavailable; };
        if self.measurement_unavailable || !self.calls.is_empty() { return Unavailable; }
        let reasoning = match usage.get("output_tokens_details") {
            Some(details) if details.is_object() => details.get("reasoning_tokens"),
            Some(_) => return Unavailable,
            None => None,
        };
        // Summary text is not the hidden reasoning that output_tokens bills.
        // Without a trustworthy breakdown, only a pure text answer is paired.
        match reasoning {
            Some(count) => match count.as_i64().filter(|n| *n >= 0 && *n <= total) {
                Some(count) => Text(total - count),
                None => Unavailable,
            },
            None if !self.saw_reasoning && !self.texts.is_empty() => Text(total),
            None => Unavailable,
        }
    }

    fn item(&mut self, index: usize, item: &Value, out: &mut Vec<Result<LlmEvent, LlmError>>) {
        match item["type"].as_str() {
            Some("function_call") => self.call(index, item, out),
            Some("reasoning") => {
                self.saw_reasoning = true;
                if let Some(summary) = item["summary"].as_array() {
                    for (part, text) in summary.iter().enumerate() {
                        if let Some(text) = text["text"].as_str().filter(|s| !s.is_empty()) {
                            self.thought((index, part, true), text, false, out);
                        }
                    }
                }
                if let Some(content) = item["content"].as_array() {
                    for (part, text) in content.iter().enumerate() {
                        if text["type"] == "reasoning_text" {
                            if let Some(text) = text["text"].as_str() { self.thought((index, part, false), text, false, out); }
                        }
                    }
                }
                self.reasoning.insert(index, item.clone());
            }
            Some("message") => {
                if let Some(content) = item["content"].as_array() {
                    for (part, text) in content.iter().enumerate() {
                        if text["type"] != "output_text" { self.measurement_unavailable = true; }
                        if text["type"] == "refusal" {
                            self.failed = true;
                            out.push(Err(LlmError::Forbidden(text["refusal"].as_str().unwrap_or("The provider withheld the answer").into())));
                        } else if let Some(text) = text["text"].as_str() {
                            self.text(index, part, text, out);
                        } else {
                            self.measurement_unavailable = true;
                        }
                    }
                } else {
                    self.measurement_unavailable = true;
                }
            }
            _ => self.measurement_unavailable = true,
        }
    }

    fn complete(&mut self, response: &Value, incomplete: bool, out: &mut Vec<Result<LlmEvent, LlmError>>) {
        if let Some(items) = response["output"].as_array() {
            for (index, item) in items.iter().enumerate() { self.item(index, item, out); }
            // A matching part elsewhere does not confirm a streamed part that
            // disappeared or changed modality in the completed output.
            if self.texts.iter().any(|(&(index, part), text)| {
                items.get(index).filter(|item| item["type"] == "message")
                    .and_then(|item| item["content"].as_array()).and_then(|content| content.get(part))
                    .filter(|part| part["type"] == "output_text").and_then(|part| part["text"].as_str()) != Some(text.as_str())
            }) { self.measurement_unavailable = true; }
            let final_text: String = items.iter().filter(|item| item["type"] == "message")
                .filter_map(|item| item["content"].as_array()).flatten()
                .filter(|part| part["type"] == "output_text").filter_map(|part| part["text"].as_str()).collect();
            if final_text != self.emitted_text { self.measurement_unavailable = true; }
        } else {
            // Without completed items, unseen reasoning or modalities cannot be ruled out.
            self.measurement_unavailable = true;
        }
        if self.failed { return; }
        if let Some(error) = crate::openai::stream_error(response) {
            self.failed = true;
            out.push(Err(LlmError::Stream(error)));
            return;
        }
        if incomplete && response.pointer("/incomplete_details/reason").and_then(Value::as_str) != Some("max_output_tokens") {
            self.failed = true;
            out.push(Err(LlmError::Stream(format!("Incomplete response: {}", response["incomplete_details"]))));
            return;
        }
        if let Some(usage) = response.get("usage").filter(|u| u.is_object()) {
            let completion_measurement = self.completion_measurement(usage);
            if let CompletionMeasurement::Text(tokens) = completion_measurement {
                if !self.model.is_empty() {
                    crate::tokenizer::note_measured(&self.model, &self.emitted_text, tokens as usize);
                }
            }
            out.push(Ok(LlmEvent::Usage(Usage {
                prompt: usage["input_tokens"].as_i64(), completion: usage["output_tokens"].as_i64(),
                completion_measurement,
                cached: usage.pointer("/input_tokens_details/cached_tokens").and_then(Value::as_i64),
                cost: usage["cost"].as_f64(),
                cost_details: usage.pointer("/cost_details/upstream_inference_cost").and_then(Value::as_f64)
                    .map(|upstream_cost| CostDetails { upstream_cost: Some(upstream_cost) }),
                ..Default::default()
            })));
        }
        if !self.reasoning.is_empty() {
            out.push(Ok(LlmEvent::Replay(json!({"protocol": "responses", "reasoning": self.reasoning.values().collect::<Vec<_>>()}))));
        }
        self.terminal = true;
        out.push(Ok(LlmEvent::Done(if incomplete { FinishReason::Length } else if !self.calls.is_empty() { FinishReason::ToolUse } else { FinishReason::Stop })));
    }

    fn event(&mut self, value: Value) -> Vec<Result<LlmEvent, LlmError>> {
        let mut out = Vec::new();
        if self.failed || self.terminal { return out; }
        let index = value["output_index"].as_u64().unwrap_or(0) as usize;
        let kind = value["type"].as_str().or_else(|| (value["object"] == "response").then_some("response"));
        match kind {
            Some("response.output_text.delta") => {
                if let Some(text) = value["delta"].as_str() {
                    self.texts.entry((index, value["content_index"].as_u64().unwrap_or(0) as usize)).or_default().push_str(text);
                    self.emitted_text.push_str(text);
                    out.push(Ok(LlmEvent::TextDelta(text.into())));
                } else {
                    self.measurement_unavailable = true;
                }
            }
            Some("response.output_text.done") => {
                if let Some(text) = value["text"].as_str() {
                    self.text(index, value["content_index"].as_u64().unwrap_or(0) as usize, text, &mut out);
                } else {
                    self.measurement_unavailable = true;
                }
            }
            Some("response.content_part.added" | "response.content_part.done") => {
                if value["part"]["type"] != "output_text" { self.measurement_unavailable = true; }
            }
            Some("response.reasoning_summary_text.delta" | "response.reasoning_text.delta"
                | "response.reasoning_summary_text.done" | "response.reasoning_text.done") => {
                self.saw_reasoning = true;
                let summary = kind.is_some_and(|kind| kind.starts_with("response.reasoning_summary_"));
                let part = value[if summary { "summary_index" } else { "content_index" }].as_u64().unwrap_or(0) as usize;
                let delta = kind.is_some_and(|kind| kind.ends_with(".delta"));
                if let Some(text) = value[if delta { "delta" } else { "text" }].as_str() {
                    self.thought((index, part, summary), text, delta, &mut out);
                }
            }
            Some("response.reasoning_summary_part.added" | "response.reasoning_summary_part.done") => {
                self.saw_reasoning = true;
                if let Some(text) = value["part"]["text"].as_str() {
                    let part = value["summary_index"].as_u64().unwrap_or(0) as usize;
                    self.thought((index, part, true), text, false, &mut out);
                }
            }
            Some("response.output_item.added" | "response.output_item.done") => self.item(index, &value["item"], &mut out),
            Some("response.function_call_arguments.delta") => {
                self.measurement_unavailable = true;
                let call = self.calls.entry(index).or_default();
                if let Some(args) = value["delta"].as_str() {
                    call.arguments.push_str(args);
                    out.push(Ok(LlmEvent::ToolCallDelta { index, id: call.id.clone(), name: call.name.clone(), args_delta: args.into() }));
                }
            }
            Some("response.function_call_arguments.done") => self.call(index, &value, &mut out),
            Some("response.completed" | "response.incomplete") => self.complete(&value["response"], value["type"] == "response.incomplete", &mut out),
            Some("response.failed" | "error") => {
                self.failed = true;
                let error = value.get("error").or_else(|| value.pointer("/response/error")).unwrap_or(&value);
                out.push(Err(LlmError::Stream(error["message"].as_str().map(str::to_string).unwrap_or_else(|| error.to_string()))));
            }
            Some("response") => match value["status"].as_str() {
                Some("completed" | "incomplete") => self.complete(&value, value["status"] == "incomplete", &mut out),
                Some("failed") => { self.failed = true; out.push(Err(LlmError::Stream(value["error"].to_string()))); }
                _ => {}
            },
            _ => {
                // New output events may bill content that never becomes a TextDelta.
                if value["type"].as_str().is_some_and(|kind| kind.starts_with("response.")
                    && !matches!(kind, "response.created" | "response.queued" | "response.in_progress" | "response.output_text.annotation.added")) {
                    self.measurement_unavailable = true;
                }
                if let Some(error) = crate::openai::stream_error(&value) { self.failed = true; out.push(Err(LlmError::Stream(error))); }
            }
        }
        out
    }

    fn payloads(&mut self, payloads: Vec<String>) -> Vec<Result<LlmEvent, LlmError>> {
        let mut out = Vec::new();
        for payload in payloads {
            if self.failed || self.terminal { break; }
            if payload.trim() == "[DONE]" { continue; }
            match serde_json::from_str::<Value>(&payload) {
                Ok(value) => { self.malformed.ok(); out.extend(self.event(value)); }
                Err(_) if self.malformed.bad() => { self.failed = true; out.push(Err(self.malformed.message())); }
                Err(_) => {}
            }
        }
        out
    }
}

impl WireDecoder for Decoder {
    fn feed(&mut self, bytes: &[u8]) -> Vec<Result<LlmEvent, LlmError>> {
        if self.whole.is_none() {
            self.whole = bytes.iter().find(|b| !b.is_ascii_whitespace()).map(|b| *b == b'{');
        }
        if self.whole == Some(true) { self.reply.extend_from_slice(bytes); return Vec::new(); }
        let payloads = self.sse.feed(bytes);
        self.payloads(payloads)
    }

    fn finish(&mut self) -> Vec<Result<LlmEvent, LlmError>> {
        let payloads = if self.whole == Some(true) { vec![String::from_utf8_lossy(&self.reply).into_owned()] } else { self.sse.finish() };
        self.payloads(payloads)
    }

    fn saw_terminal(&self) -> bool { self.terminal || self.failed }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ApiProtocol, Endpoint, LlmBackend, ToolCall};
    use futures::StreamExt;

    fn client(url: &str) -> Client {
        Client::new(Endpoint::new(ApiProtocol::OpenAi, url, None), "gpt-6.1-sol")
    }

    fn sse(events: &[Value]) -> String {
        events.iter().map(|v| format!("event: {}\ndata: {v}\n\n", v["type"].as_str().unwrap_or(""))).collect()
    }

    fn completed(output: Value) -> Value {
        json!({"type": "response.completed", "response": {
            "status": "completed", "output": output,
            "usage": {"input_tokens": 50, "output_tokens": 9, "input_tokens_details": {"cached_tokens": 40}}
        }})
    }

    fn text_reply(text: &str) -> Value {
        completed(json!([{"type": "message", "content": [{"type": "output_text", "text": text}]}]))
    }

    fn thought(events: &[LlmEvent]) -> String {
        events.iter().filter_map(|event| if let LlmEvent::ReasoningDelta(text) = event { Some(text.as_str()) } else { None }).collect()
    }

    fn replay_message(items: Value) -> ChatMessage {
        let mut message = ChatMessage::assistant("previous answer");
        message.replay = Some(json!({"protocol": "responses", "reasoning": items}));
        message
    }

    fn resource_error() -> String {
        json!({"error": {"message": format!("OpenAI Responses bad request: {RESOURCE_MISMATCH} [trace_id=00112233445566778899aabbccddeeff]")}}).to_string()
    }

    #[test]
    fn standard_response_objects_preserve_reasoning_text_usage_and_stop() {
        let response = json!({"object":"response","status":"completed","output":[
            {"type":"reasoning","summary":[{"type":"summary_text","text":"plan"}],"encrypted_content":"opaque"},
            {"type":"message","content":[{"type":"output_text","text":"answer"}]}
        ],"usage":{"input_tokens":50,"output_tokens":120,"output_tokens_details":{"reasoning_tokens":80}}});
        for chunk in [1, 7, response.to_string().len()] {
            let events = decode(&response.to_string(), chunk);
            assert_eq!(thought(&events), "plan");
            assert_eq!(answer(&events), "answer");
            assert_eq!(reported_usage(&events).completion_measurement, CompletionMeasurement::Text(40));
            assert!(events.contains(&LlmEvent::Done(FinishReason::Stop)));
        }
    }

    #[test]
    fn reasoning_done_events_are_read_without_delta_or_final_copies() {
        let wire = sse(&[
            json!({"type":"response.reasoning_summary_text.done","output_index":0,"summary_index":0,"text":"summary"}),
            json!({"type":"response.reasoning_summary_part.done","output_index":0,"summary_index":1,"part":{"type":"summary_text","text":"part"}}),
            json!({"type":"response.reasoning_text.done","output_index":0,"content_index":0,"text":"full reasoning"}),
            completed(json!([{"type":"reasoning","summary":[]},{"type":"message","content":[{"type":"output_text","text":"answer"}]}])),
        ]);
        let events = decode(&wire, 1);
        assert_eq!(thought(&events), "summarypartfull reasoning");
        assert_eq!(answer(&events), "answer");
    }

    #[test]
    fn reasoning_prefixes_are_completed_once_and_missing_deltas_do_not_hide_final_text() {
        let reason = json!({"type":"reasoning","summary":[{"type":"summary_text","text":"plan"}]});
        let wire = sse(&[
            json!({"type":"response.reasoning_summary_text.delta","output_index":0,"summary_index":0,"delta":"pl"}),
            json!({"type":"response.reasoning_summary_text.done","output_index":0,"summary_index":0,"text":"plan"}),
            json!({"type":"response.reasoning_summary_part.done","output_index":0,"summary_index":0,"part":{"type":"summary_text","text":"plan"}}),
            json!({"type":"response.output_item.done","output_index":0,"item":reason}),
            completed(json!([reason])),
        ]);
        assert_eq!(thought(&decode(&wire, 1)), "plan");
        let missing = sse(&[
            json!({"type":"response.reasoning_summary_text.delta","output_index":0,"summary_index":0}),
            completed(json!([{"type":"reasoning","summary":[{"type":"summary_text","text":"plan"}]}])),
        ]);
        assert_eq!(thought(&decode(&missing, 7)), "plan");
    }

    #[test]
    fn readable_reasoning_content_is_not_confused_with_summary_parts() {
        let reason = json!({"type":"reasoning","summary":[],"content":[
            {"type":"reasoning_text","text":"first"},{"type":"reasoning_text","text":"second"}
        ],"encrypted_content":"opaque"});
        let reply = completed(json!([reason]));
        for wire in [sse(std::slice::from_ref(&reply)), whole_json(&reply)] {
            assert_eq!(thought(&decode(&wire, 1)), "firstsecond");
        }
        let wire = sse(&[
            json!({"type":"response.reasoning_text.delta","output_index":0,"content_index":0,"delta":"fir"}),
            json!({"type":"response.reasoning_text.done","output_index":0,"content_index":0,"text":"first"}),
            json!({"type":"response.reasoning_text.delta","output_index":0,"content_index":1,"delta":"second"}),
            reply,
        ]);
        assert_eq!(thought(&decode(&wire, 7)), "firstsecond");
    }

    #[test]
    fn a_late_suffix_of_an_earlier_part_is_not_a_calibration_pair() {
        let mut decoder = Decoder::default();
        let mut events = decoder.event(json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"First par"}));
        events.extend(decoder.event(json!({"type":"response.output_text.delta","output_index":0,"content_index":1,"delta":"Second part."})));
        events.extend(decoder.event(completed(json!([{"type":"message","content":[
            {"type":"output_text","text":"First part."},{"type":"output_text","text":"Second part."}
        ]}]))));
        assert!(events.iter().any(|event| matches!(event, Ok(LlmEvent::Usage(usage))
            if usage.completion_measurement == CompletionMeasurement::Unavailable)));
    }

    #[test]
    fn completed_without_final_output_cannot_prove_a_calibration_pair() {
        for details in [json!({}), json!({"reasoning_tokens": 80})] {
            let mut decoder = Decoder::default();
            decoder.event(json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"visible answer"}));
            let events = decoder.event(json!({"type":"response.completed","response":{
                "status":"completed","usage":{"output_tokens":120,"output_tokens_details":details}
            }}));
            assert!(events.iter().any(|event| matches!(event, Ok(LlmEvent::Usage(usage))
                if usage.completion == Some(120) && usage.completion_measurement == CompletionMeasurement::Unavailable)));
            assert!(events.iter().any(|event| matches!(event, Ok(LlmEvent::Done(FinishReason::Stop)))));
        }
    }

    fn measured_reply(output: Value, reasoning_tokens: Value) -> Value {
        let mut reply = completed(output);
        reply["response"]["usage"]["output_tokens"] = json!(120);
        reply["response"]["usage"]["output_tokens_details"] = json!({"reasoning_tokens": reasoning_tokens});
        reply
    }

    fn whole_json(reply: &Value) -> String {
        let mut response = reply["response"].clone();
        response["object"] = json!("response");
        response.to_string()
    }

    fn decode(wire: &str, chunk_size: usize) -> Vec<LlmEvent> {
        let mut decoder = Decoder::default();
        let mut events = Vec::new();
        for bytes in wire.as_bytes().chunks(chunk_size) { events.extend(decoder.feed(bytes)); }
        events.extend(decoder.finish());
        events.into_iter().map(Result::unwrap).collect()
    }

    fn answer(events: &[LlmEvent]) -> String {
        events.iter().filter_map(|e| if let LlmEvent::TextDelta(s) = e { Some(s.as_str()) } else { None }).collect()
    }

    fn reported_usage(events: &[LlmEvent]) -> Usage {
        let mut usages = events.iter().filter_map(|e| if let LlmEvent::Usage(u) = e { Some(*u) } else { None });
        let usage = usages.next().expect("Responses usage");
        assert!(usages.next().is_none(), "usage must not be repeated");
        usage
    }

    #[test]
    fn summaries_and_answers_measure_only_text_in_sse_and_whole_json() {
        let reason = json!({"type":"reasoning", "id":"rs_1", "summary":[{"type":"summary_text", "text":"plan"}], "encrypted_content":"opaque"});
        let message = json!({"type":"message", "content":[{"type":"output_text", "text":"answer"}]});
        let mut reply = measured_reply(json!([reason, message]), json!(80));
        reply["response"]["usage"]["cost"] = json!(0.125);
        reply["response"]["usage"]["cost_details"] = json!({"upstream_inference_cost":0.0625});
        let streamed = sse(&[
            json!({"type":"response.created", "response":{"status":"in_progress", "output":[]}}),
            json!({"type":"response.output_item.added", "output_index":0, "item":{"type":"reasoning", "summary":[]}}),
            json!({"type":"response.reasoning_summary_text.delta", "output_index":0, "summary_index":0, "delta":"plan"}),
            json!({"type":"response.output_item.done", "output_index":0, "item":reason}),
            json!({"type":"response.content_part.added", "output_index":1, "content_index":0, "part":{"type":"output_text", "text":""}}),
            json!({"type":"response.output_text.delta", "output_index":1, "content_index":0, "delta":"answer"}),
            reply.clone(),
        ]);
        for wire in [streamed, whole_json(&reply)] {
            for chunk_size in [1, 7, 512, wire.len()] {
                let events = decode(&wire, chunk_size);
                let thought: String = events.iter().filter_map(|e| if let LlmEvent::ReasoningDelta(s) = e { Some(s.as_str()) } else { None }).collect();
                assert_eq!(answer(&events), "answer");
                assert_eq!(thought, "plan");
                let usage = reported_usage(&events);
                assert_eq!(usage.completion_measurement, CompletionMeasurement::Text(40));
                assert_eq!(usage.completion, Some(120));
                assert_eq!(usage.prompt, Some(50));
                assert_eq!(usage.cached, Some(40));
                assert_eq!(usage.cost, Some(0.125));
                assert_eq!(usage.cost_details, Some(CostDetails { upstream_cost: Some(0.0625) }));
                assert!(events.iter().any(|e| matches!(e, LlmEvent::Replay(v) if v["reasoning"][0] == reason)));
                assert!(events.contains(&LlmEvent::Done(FinishReason::Stop)));
                assert_eq!(events.iter().filter(|e| matches!(e, LlmEvent::Done(_))).count(), 1);
            }
        }
    }

    #[test]
    fn hidden_or_empty_reasoning_without_a_count_is_unavailable() {
        let message = json!({"type":"message", "content":[{"type":"output_text", "text":"answer"}]});
        for summary in [None, Some(json!([])), Some(json!([{"type":"summary_text", "text":""}]))] {
            let mut reason = json!({"type":"reasoning", "encrypted_content":"opaque"});
            if let Some(summary) = summary { reason["summary"] = summary; }
            let reply = completed(json!([reason, message]));
            for wire in [sse(std::slice::from_ref(&reply)), whole_json(&reply)] {
                let events = decode(&wire, 7);
                assert_eq!(answer(&events), "answer");
                assert!(!events.iter().any(|e| matches!(e, LlmEvent::ReasoningDelta(_))));
                let usage = reported_usage(&events);
                assert_eq!(usage.completion_measurement, CompletionMeasurement::Unavailable);
                assert_eq!(usage.completion, Some(9));
                assert_eq!(usage.cached, Some(40));
            }
        }
        for kind in ["response.reasoning_summary_text.delta", "response.reasoning_text.delta"] {
            let wire = sse(&[
                json!({"type":kind, "output_index":1, "summary_index":0, "delta":""}),
                text_reply("answer"),
            ]);
            assert_eq!(reported_usage(&decode(&wire, 7)).completion_measurement, CompletionMeasurement::Unavailable);
        }
    }

    #[test]
    fn reasoning_breakdowns_require_bounded_nonnegative_i64_counts() {
        let output = json!([{"type":"message", "content":[{"type":"output_text", "text":"answer"}]}]);
        for count in [json!(-1), json!(121), json!(u64::MAX), json!("80"), json!(80.0), Value::Null, json!(true)] {
            let reply = measured_reply(output.clone(), count.clone());
            for wire in [sse(std::slice::from_ref(&reply)), whole_json(&reply)] {
                let usage = reported_usage(&decode(&wire, 7));
                assert_eq!(usage.completion_measurement, CompletionMeasurement::Unavailable, "count {count}");
                assert_eq!(usage.completion, Some(120));
                assert_eq!(usage.cached, Some(40));
            }
        }
        for details in [Value::Null, json!([]), json!("bad"), json!(80)] {
            let mut reply = measured_reply(output.clone(), json!(80));
            reply["response"]["usage"]["output_tokens_details"] = details.clone();
            let usage = reported_usage(&decode(&sse(&[reply]), 7));
            assert_eq!(usage.completion_measurement, CompletionMeasurement::Unavailable, "details {details}");
            assert_eq!(usage.completion, Some(120));
        }
        for total in [json!(-1), json!(u64::MAX), json!("120"), json!(120.0), Value::Null] {
            let mut reply = measured_reply(output.clone(), json!(0));
            reply["response"]["usage"]["output_tokens"] = total.clone();
            let usage = reported_usage(&decode(&sse(&[reply]), 7));
            assert_eq!(usage.completion_measurement, CompletionMeasurement::Unavailable, "total {total}");
            assert_eq!(usage.completion, total.as_i64());
        }
        for (total, count, expected) in [(120, 0, 120), (120, 120, 0), (0, 0, 0), (i64::MAX, i64::MAX, 0), (i64::MAX, 0, i64::MAX)] {
            let mut reply = measured_reply(output.clone(), json!(count));
            reply["response"]["usage"]["output_tokens"] = json!(total);
            let usage = reported_usage(&decode(&sse(&[reply]), 7));
            assert_eq!(usage.completion_measurement, CompletionMeasurement::Text(expected));
            assert_eq!(usage.completion, Some(total));
        }
    }

    #[test]
    fn pure_output_text_without_a_reasoning_count_uses_the_total() {
        for details in [None, Some(json!({})), Some(json!({"accepted_prediction_tokens":0}))] {
            let mut reply = text_reply("answer");
            if let Some(details) = details { reply["response"]["usage"]["output_tokens_details"] = details; }
            let streamed = sse(&[
                json!({"type":"response.output_text.delta", "output_index":0, "content_index":0, "delta":"answer"}),
                reply.clone(),
            ]);
            for wire in [streamed, whole_json(&reply)] {
                let events = decode(&wire, 7);
                assert_eq!(answer(&events), "answer");
                let usage = reported_usage(&events);
                assert_eq!(usage.completion_measurement, CompletionMeasurement::Text(9));
                assert_eq!(usage.completion, Some(9));
                assert_eq!(usage.cached, Some(40));
            }
        }
    }

    #[test]
    fn tool_framing_is_unavailable_even_with_a_valid_reasoning_count() {
        let message = json!({"type":"message", "content":[{"type":"output_text", "text":"answer"}]});
        let call = json!({"type":"function_call", "call_id":"call_1", "name":"read_file", "arguments":"{}"});
        let reply = measured_reply(json!([message, call]), json!(80));
        for wire in [sse(std::slice::from_ref(&reply)), whole_json(&reply)] {
            let events = decode(&wire, 7);
            let usage = reported_usage(&events);
            assert_eq!(usage.completion_measurement, CompletionMeasurement::Unavailable);
            assert_eq!(usage.completion, Some(120));
            assert_eq!(usage.cached, Some(40));
            assert!(events.iter().any(|e| matches!(e, LlmEvent::ToolCallDelta { id: Some(id), args_delta, .. } if id == "call_1" && args_delta == "{}")));
            assert!(events.contains(&LlmEvent::Done(FinishReason::ToolUse)));
        }
        let wire = sse(&[
            json!({"type":"response.function_call_arguments.delta", "output_index":1, "delta":"{}"}),
            measured_reply(json!([message]), json!(80)),
        ]);
        assert_eq!(reported_usage(&decode(&wire, 7)).completion_measurement, CompletionMeasurement::Unavailable);
    }

    #[test]
    fn unknown_items_and_other_modalities_cannot_measure_text() {
        let text = json!({"type":"output_text", "text":"answer"});
        let message = json!({"type":"message", "content":[text]});
        for item in [json!({"type":"image_generation_call", "result":"opaque"}), json!({"type":"future_item"}), json!({})] {
            let reply = measured_reply(json!([message, item]), json!(80));
            for wire in [sse(std::slice::from_ref(&reply)), whole_json(&reply)] {
                let usage = reported_usage(&decode(&wire, 7));
                assert_eq!(usage.completion_measurement, CompletionMeasurement::Unavailable);
                assert_eq!(usage.completion, Some(120));
            }
        }
        for part in [json!({"type":"output_audio", "data":"opaque"}), json!({"type":"future_text", "text":"extra"}), json!({"text":"extra"})] {
            let reply = measured_reply(json!([{"type":"message", "content":[text, part]}]), json!(80));
            for wire in [sse(std::slice::from_ref(&reply)), whole_json(&reply)] {
                let usage = reported_usage(&decode(&wire, 7));
                assert_eq!(usage.completion_measurement, CompletionMeasurement::Unavailable);
                assert_eq!(usage.completion, Some(120));
            }
        }
        for frame in [
            json!({"type":"response.output_item.added", "output_index":1, "item":{"type":"future_item"}}),
            json!({"type":"response.content_part.added", "output_index":1, "content_index":0, "part":{"type":"output_audio"}}),
            json!({"type":"response.content_part.done", "output_index":1, "content_index":0, "part":{"type":"future_text", "text":"extra"}}),
            json!({"type":"response.audio.delta", "output_index":1, "delta":"opaque"}),
        ] {
            let wire = sse(&[frame, measured_reply(json!([message]), json!(80))]);
            assert_eq!(reported_usage(&decode(&wire, 7)).completion_measurement, CompletionMeasurement::Unavailable);
        }
    }

    #[test]
    fn streamed_prefixes_are_completed_once_by_item_done_and_final_output() {
        let reason = json!({"type":"reasoning", "summary":[], "encrypted_content":"opaque"});
        let message = json!({"type":"message", "content":[{"type":"output_text", "text":"Привет, мир!"}]});
        for done_text in [None, Some("Привет, "), Some("Привет, мир!")] {
            let mut frames = vec![
                json!({"type":"response.output_item.added", "output_index":0, "item":{"type":"reasoning", "summary":[]}}),
                json!({"type":"response.output_text.delta", "output_index":1, "content_index":0, "delta":"При"}),
                json!({"type":"response.output_text.delta", "output_index":1, "content_index":0, "delta":"вет"}),
            ];
            if let Some(text) = done_text {
                let done = json!({"type":"response.output_item.done", "output_index":1, "item":{"type":"message", "content":[{"type":"output_text", "text":text}]}});
                frames.push(done.clone());
                frames.push(done);
            }
            let reply = measured_reply(json!([reason, message]), json!(80));
            frames.push(reply.clone());
            frames.push(reply);
            let wire = sse(&frames);
            for chunk_size in [1, 7, 512, wire.len()] {
                let events = decode(&wire, chunk_size);
                assert_eq!(answer(&events), "Привет, мир!");
                let expected_deltas = if done_text == Some("Привет, ") { 4 } else { 3 };
                assert_eq!(events.iter().filter(|e| matches!(e, LlmEvent::TextDelta(_))).count(), expected_deltas);
                let usage = reported_usage(&events);
                assert_eq!(usage.completion_measurement, CompletionMeasurement::Text(40));
                assert_eq!(usage.completion, Some(120));
                assert_eq!(usage.cached, Some(40));
                assert_eq!(events.iter().filter(|e| matches!(e, LlmEvent::Done(_))).count(), 1);
            }
        }
    }

    #[test]
    fn incompatible_final_text_is_not_repeated_or_measured() {
        for final_text in ["different", "show", ""] {
            for item_done in [false, true] {
                let mut frames = vec![json!({"type":"response.output_text.delta", "output_index":0, "content_index":0, "delta":"shown"})];
                let message = json!({"type":"message", "content":[{"type":"output_text", "text":final_text}]});
                if item_done { frames.push(json!({"type":"response.output_item.done", "output_index":0, "item":message})); }
                frames.push(measured_reply(json!([message]), json!(80)));
                let events = decode(&sse(&frames), 7);
                assert_eq!(answer(&events), "shown");
                let usage = reported_usage(&events);
                assert_eq!(usage.completion_measurement, CompletionMeasurement::Unavailable);
                assert_eq!(usage.completion, Some(120));
                assert!(events.contains(&LlmEvent::Done(FinishReason::Stop)));
            }
        }
        let wire = sse(&[
            json!({"type":"response.output_text.delta", "output_index":0, "content_index":0, "delta":"shown"}),
            json!({"type":"response.output_item.done", "output_index":0, "item":{"type":"message", "content":[{"type":"output_text", "text":"different"}]}}),
            measured_reply(json!([{"type":"message", "content":[{"type":"output_text", "text":"shown suffix"}]}]), json!(80)),
        ]);
        let events = decode(&wire, 7);
        assert_eq!(answer(&events), "shown suffix");
        assert_eq!(reported_usage(&events).completion_measurement, CompletionMeasurement::Unavailable);
    }

    #[test]
    fn every_streamed_text_part_must_be_confirmed_by_present_final_output() {
        let message = json!({"type":"message", "content":[{"type":"output_text", "text":"answer"}]});
        for (index, part, output) in [
            (0, 0, json!([])),
            (0, 0, json!([{"type":"message", "content":[]}])),
            (1, 0, json!([message])),
            (0, 1, json!([message])),
            (0, 0, Value::Null),
        ] {
            let wire = sse(&[
                json!({"type":"response.output_text.delta", "output_index":index, "content_index":part, "delta":"streamed"}),
                measured_reply(output, json!(80)),
            ]);
            let usage = reported_usage(&decode(&wire, 7));
            assert_eq!(usage.completion_measurement, CompletionMeasurement::Unavailable, "part ({index}, {part})");
            assert_eq!(usage.completion, Some(120));
        }
    }

    #[test]
    fn the_screenshot_refusal_requests_responses_but_other_errors_do_not() {
        let error = r#"{"error":{"message":"模型 gpt-6.1-sol 不支持 /v1/chat/completions 接口，请改用 /v1/responses (OpenAI Responses API)。 Model gpt-6.1-sol does not support /v1/chat/completions, please use /v1/responses instead.","type":"new_api_error"}}"#;
        assert!(required_by(error));
        for error in [
            "Insufficient credits", "invalid api key", "context length exceeded", "the model does not support tools",
            "Error at /responses", "Unknown tool argument /v1/responses in /v1/chat/completions",
        ] { assert!(!required_by(error), "{error}"); }
    }

    #[test]
    fn only_the_exact_azure_resource_mismatch_is_recoverable() {
        let actual = "OpenAI Responses bad request: The requested item was created under a different Azure OpenAI resource. Use the same resource that created the item to access it.";
        assert!(resource_mismatch(actual));
        assert!(resource_mismatch(RESOURCE_MISMATCH));
        assert!(resource_mismatch(&json!({"error":{"message":actual}}).to_string()));
        assert!(resource_mismatch(&format!("{actual} [trace_id=abc123]")));
        assert!(resource_mismatch(&format!("{RESOURCE_MISMATCH}\n[trace_id=abc-123]")));
        assert!(resource_mismatch(&resource_error()));
        for error in [
            "The requested item was created under a different resource.",
            "Invalid encrypted_content", "Unknown reasoning id", "context length exceeded",
            &format!("Tool error: {actual}"), &format!("{actual} Another error occurred."),
            &format!("{actual} [trace_id=]"), &format!("{actual} [trace_id=abc123] Another error occurred."),
        ] { assert!(!resource_mismatch(error), "{error}"); }
    }

    #[test]
    fn visible_replay_prefers_saved_reasoning_and_extracts_only_plaintext_once() {
        let mut message = replay_message(json!([{
            "type":"reasoning", "id":"rs_1", "encrypted_content":"sealed, not readable",
            "summary":[{"type":"summary_text","text":"a summary"},{"text":"a summary"}],
            "content":[
                {"type":"reasoning_text","text":"a summary"},
                {"type":"reasoning_text","text":"more detail"},
                {"type":"encrypted","text":"not reasoning text"},
                {"type":"reasoning_text","text":17}
            ]
        }]));
        message.reasoning = Some("the saved visible reasoning".into());
        let sent = input(&[message.clone()], true);
        assert_eq!(sent, json!([
            {"role":"assistant","content":"Earlier assistant reasoning (visible text):\nthe saved visible reasoning"},
            {"role":"assistant","content":"previous answer"}
        ]));
        for empty in [None, Some(String::new()), Some(" \n".into())] {
            message.reasoning = empty;
            assert_eq!(input(&[message.clone()], true), json!([
                {"role":"assistant","content":"Earlier assistant reasoning (visible text):\na summary\nmore detail"},
                {"role":"assistant","content":"previous answer"}
            ]));
        }
        message.replay.as_mut().unwrap()["protocol"] = json!("anthropic");
        assert_eq!(input(&[message], true), json!([{"role":"assistant","content":"previous answer"}]));
    }

    #[test]
    fn a_supported_default_without_a_preset_still_requests_a_summary() {
        let c = client("http://localhost:1234/v1/responses").with_profile(crate::thinking::ThinkingProfile {
            presets: vec!["low".into(), "high".into(), "max".into()],
            protocol: crate::thinking::ThinkingProtocol::ReasoningEffort,
            supported: true, default_preset: None,
        });
        let options = TurnOptions { thinking: ThinkingEffort::Default, ..Default::default() };
        let sent = body(&c, &[ChatMessage::user("hello")], &[], &options);
        assert_eq!(sent["reasoning"], json!({"summary":"auto"}));
        for profile in [crate::thinking::ThinkingProfile::unsupported(), crate::thinking::ThinkingProfile::unreported()] {
            c.set_profile(profile);
            assert!(body(&c, &[], &[], &options).get("reasoning").is_none());
        }
        assert!(body(&client("http://localhost:1234/v1"), &[], &[], &options).get("reasoning").is_none());
    }

    #[test]
    fn requesting_a_summary_never_promotes_an_explicit_disabled_effort() {
        let c = client("http://localhost:1234/v1/responses").with_profile(crate::thinking::ThinkingProfile {
            presets: vec!["low".into(), "high".into()],
            protocol: crate::thinking::ThinkingProtocol::ReasoningEffort,
            supported: true, default_preset: Some("high".into()),
        });
        for options in [
            TurnOptions { thinking: ThinkingEffort::Off, ..Default::default() },
            TurnOptions { custom_effort: Some("none".into()), ..Default::default() },
            TurnOptions { custom_effort: Some("off".into()), ..Default::default() },
        ] {
            assert_eq!(body(&c, &[], &[], &options)["reasoning"], json!({"effort":"none", "summary":"auto"}));
        }
    }

    #[test]
    fn stateless_input_preserves_messages_tools_images_and_encrypted_reasoning() {
        let c = client("http://localhost:1234/v1");
        let mut user = ChatMessage::user("read it");
        user.images.push("data:image/png;base64,AAAA".into());
        let mut assistant = ChatMessage::assistant("opening file");
        assistant.tool_calls = vec![ToolCall { id: "call_one".into(), name: "read_file".into(), args_json: r#"{"path":"a.rs"}"#.into() }];
        let reasoning = json!({"type":"reasoning", "id":"rs_1", "summary":[], "encrypted_content":"opaque"});
        assistant.replay = Some(json!({"protocol":"responses", "reasoning":[reasoning]}));
        let tool = ToolSpec { name: "read_file".into(), description: "Read a file".into(), parameters_json: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#.into() };
        let options = TurnOptions { max_tokens: Some(100), custom_effort: Some("max".into()), temperature: Some(0.6), top_k: Some(20), ..Default::default() };
        let sent = body(&c, &[ChatMessage::system("rules"), user, assistant, ChatMessage::tool_result("call_one", "file contents")], &[tool], &options);
        assert_eq!(sent["store"], false);
        assert_eq!(sent["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(sent["input"][0]["role"], "system");
        assert_eq!(sent["input"][1]["content"][1]["type"], "input_image");
        assert_eq!(sent["input"][2], reasoning);
        assert_eq!(sent["input"][4]["call_id"], "call_one");
        assert_eq!(sent["input"][5], json!({"type":"function_call_output", "call_id":"call_one", "output":"file contents"}));
        assert_eq!(sent["tools"][0]["name"], "read_file");
        assert_eq!(sent["tools"][0]["strict"], false);
        assert_eq!(sent["max_output_tokens"], 100);
        assert_eq!(sent["reasoning"]["effort"], "max");
        for local in ["messages", "stream_options", "max_tokens", "top_k", "cache_prompt", "temperature"] {
            assert!(sent.get(local).is_none(), "{local}: {sent}");
        }
    }

    #[test]
    fn fragmented_streams_do_not_repeat_text_arguments_or_reasoning() {
        let reason = json!({"type":"reasoning", "id":"rs_1", "summary":[{"type":"summary_text", "text":"plan"}], "encrypted_content":"opaque"});
        let call = json!({"type":"function_call", "id":"fc_1", "call_id":"call_1", "name":"read_file", "arguments":"{\"path\":\"a.rs\"}"});
        let wire = sse(&[
            json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs_1","summary":[]}}),
            json!({"type":"response.reasoning_summary_text.delta","output_index":0,"summary_index":0,"delta":"plan"}),
            json!({"type":"response.output_item.done","output_index":0,"item":reason}),
            json!({"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"Привет!"}),
            json!({"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read_file","arguments":""}}),
            json!({"type":"response.function_call_arguments.delta","output_index":2,"delta":"{\"path\":"}),
            json!({"type":"response.function_call_arguments.delta","output_index":2,"delta":"\"a.rs\"}"}),
            json!({"type":"response.function_call_arguments.done","output_index":2,"arguments":"{\"path\":\"a.rs\"}"}),
            json!({"type":"response.output_item.done","output_index":2,"item":call}),
            completed(json!([reason,{"type":"message","content":[{"type":"output_text","text":"Привет!"}]},call])),
        ]);
        for chunk_size in [1, 7, 512, wire.len()] {
            let mut decoder = Decoder::default();
            let mut events = Vec::new();
            for bytes in wire.as_bytes().chunks(chunk_size) { events.extend(decoder.feed(bytes)); }
            events.extend(decoder.finish());
            let events: Vec<LlmEvent> = events.into_iter().map(Result::unwrap).collect();
            let text: String = events.iter().filter_map(|e| if let LlmEvent::TextDelta(s)=e {Some(s.as_str())} else {None}).collect();
            let thought: String = events.iter().filter_map(|e| if let LlmEvent::ReasoningDelta(s)=e {Some(s.as_str())} else {None}).collect();
            let args: String = events.iter().filter_map(|e| if let LlmEvent::ToolCallDelta{args_delta,..}=e {Some(args_delta.as_str())} else {None}).collect();
            assert_eq!(text, "Привет!", "chunk size {chunk_size}");
            assert_eq!(thought, "plan");
            assert_eq!(args, r#"{"path":"a.rs"}"#);
            assert!(events.contains(&LlmEvent::Done(FinishReason::ToolUse)));
            assert_eq!(events.iter().filter(|e| matches!(e,LlmEvent::Done(_))).count(),1);
            assert!(events.iter().any(|e| matches!(e,LlmEvent::Usage(u) if u.prompt==Some(50) && u.cached==Some(40))));
            assert!(events.iter().any(|e| matches!(e,LlmEvent::Replay(v) if v["reasoning"][0]==reason)));
        }
    }

    #[test]
    fn errors_and_incomplete_responses_are_not_clean_stops() {
        for frame in [
            json!({"type":"response.failed", "response":{"error":{"message":"quota exhausted"}}}),
            json!({"type":"error", "message":"upstream failed"}),
            json!({"type":"response.incomplete", "response":{"output":[],"incomplete_details":{"reason":"content_filter"}}}),
        ] {
            let events = Decoder::default().event(frame);
            assert!(events.iter().any(Result::is_err), "{events:?}");
            assert!(!events.iter().any(|e| matches!(e,Ok(LlmEvent::Done(_)))), "{events:?}");
        }
        let mut decoder = Decoder::default();
        let events = decoder.event(json!({"type":"response.incomplete","response":{"output":[],"incomplete_details":{"reason":"max_output_tokens"}}}));
        assert!(events.iter().any(|e| matches!(e,Ok(LlmEvent::Done(FinishReason::Length)))));
        let mut decoder = Decoder::default();
        decoder.feed(b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n");
        decoder.feed(b"data: [DONE]\n\n");
        assert!(!decoder.saw_terminal(), "DONE alone proves no completed response");
    }

    #[test]
    fn a_non_streaming_response_and_parallel_calls_use_call_ids_not_item_ids() {
        let calls = json!([
            {"type":"function_call","id":"fc_a","call_id":"call_a","name":"read_file","arguments":"{}"},
            {"type":"function_call","id":"fc_b","call_id":"call_b","name":"read_file","arguments":"{}"}
        ]);
        let response = json!({"type":"response","status":"completed","output":calls});
        let mut decoder = Decoder::default();
        decoder.feed(response.to_string().as_bytes());
        let events = decoder.finish();
        assert!(events.iter().any(|e|matches!(e,Ok(LlmEvent::ToolCallDelta{index:0,id:Some(s),..}) if s=="call_a")));
        assert!(events.iter().any(|e|matches!(e,Ok(LlmEvent::ToolCallDelta{index:1,id:Some(s),..}) if s=="call_b")));
        assert!(events.iter().any(|e|matches!(e,Ok(LlmEvent::Done(FinishReason::ToolUse)))));
    }

    #[tokio::test]
    async fn the_rejected_chat_request_switches_once_and_the_next_turn_uses_responses() {
        let (url, log) = crate::openai::test_server::serve(|req| {
            if req.path.ends_with("/chat/completions") {
                ("400 Bad Request", "application/json", json!({"error":{"message":"Model does not support /v1/chat/completions, please use /v1/responses instead."}}).to_string())
            } else {
                assert_eq!(req.path, "/v1/responses");
                assert_eq!(req.json()["store"], false);
                ("200 OK", "text/event-stream", sse(&[text_reply("Привет! Чем помочь?")]))
            }
        }).await;
        let c = client(&format!("{url}/v1"));
        for _ in 0..2 {
            let events: Vec<_> = c.stream(&[ChatMessage::system("rules"), ChatMessage::user("Привет!")], &[]).await.unwrap().collect().await;
            assert!(events.iter().any(|e| matches!(e,Ok(LlmEvent::TextDelta(s)) if s=="Привет! Чем помочь?")));
            assert!(events.iter().any(|e| matches!(e,Ok(LlmEvent::Done(FinishReason::Stop)))));
        }
        let paths: Vec<_> = log.lock().unwrap().iter().map(|r|r.path.clone()).collect();
        assert_eq!(paths, ["/v1/chat/completions", "/v1/responses", "/v1/responses"]);
        assert!(c.learned.read().responses);
        c.set_model("another-model");
        assert!(!c.learned.read().responses, "wire choice does not carry to another model");
        c.learned.write().responses = true;
        c.set_endpoint(Endpoint::new(ApiProtocol::OpenAi, "https://other.example/v1", None));
        assert!(!c.learned.read().responses, "wire choice does not carry to another provider");
    }

    #[tokio::test]
    async fn explicit_responses_addresses_work_without_doubling_the_path() {
        let (url, log) = crate::openai::test_server::serve(|req| {
            assert_eq!(req.path,"/v1/responses");
            ("200 OK", "text/event-stream", sse(&[text_reply("ok")]))
        }).await;
        let c=client(&format!("{url}/v1/responses"));
        let events:Vec<_>=c.stream(&[ChatMessage::user("hi")],&[]).await.unwrap().collect().await;
        assert!(events.iter().any(|e|matches!(e,Ok(LlmEvent::Done(FinishReason::Stop)))));
        assert_eq!(log.lock().unwrap().len(),1);
    }

    #[tokio::test]
    async fn a_rejected_key_never_switches_protocols() {
        let (url, log) = crate::openai::test_server::serve(|_| (
            "401 Unauthorized", "application/json", json!({"error":{"message":"invalid API key"}}).to_string(),
        )).await;
        let c = client(&format!("{url}/v1"));
        let result = c.stream(&[ChatMessage::user("hi")], &[]).await;
        assert!(matches!(result, Err(LlmError::Status { status: 401, .. })));
        assert!(!c.learned.read().responses);
        assert_eq!(log.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn default_summaries_reach_the_server_and_rejection_does_not_drop_reasoning() {
        for accepted in [true, false] {
            let (url, log) = crate::openai::test_server::serve(move |req| {
                assert_eq!(req.json()["reasoning"], json!({"summary":"auto"}));
                if accepted {
                    ("200 OK", "text/event-stream", sse(&[text_reply("ok")]))
                } else {
                    ("400 Bad Request", "application/json", "reasoning.summary is not supported".into())
                }
            }).await;
            let c = client(&format!("{url}/v1/responses")).with_profile(crate::thinking::ThinkingProfile {
                presets: vec!["low".into(), "high".into()],
                protocol: crate::thinking::ThinkingProtocol::ReasoningEffort,
                supported: true, default_preset: None,
            });
            let options = TurnOptions { thinking: ThinkingEffort::Default, ..Default::default() };
            let result = c.stream_with_options(&[ChatMessage::user("hi")], &[], &options).await;
            if accepted {
                let events: Vec<_> = result.unwrap().collect().await;
                assert!(events.iter().all(Result::is_ok));
            } else {
                assert!(matches!(result, Err(LlmError::Status { status: 400, .. })));
            }
            assert_eq!(log.lock().unwrap().len(), 1);
            assert!(c.learned.read().responses_dropped.is_empty());
        }
    }

    #[tokio::test]
    async fn a_resource_mismatch_retries_once_preserving_context_and_learns_visible_replay() {
        let (url, log) = crate::openai::test_server::serve(|req| {
            assert_eq!(req.method, "POST");
            assert_eq!(req.path, "/v1/responses");
            if has_bound_reasoning(&req.json()) {
                ("400 Bad Request", "application/json", resource_error())
            } else {
                ("200 OK", "text/event-stream", sse(&[
                    json!({"type":"response.reasoning_summary_text.delta", "output_index":0, "summary_index":0, "delta":"Recovered reasoning."}),
                    json!({"type":"response.reasoning_summary_text.done", "output_index":0, "summary_index":0, "text":"Recovered reasoning."}),
                    completed(json!([
                        {"type":"reasoning", "id":"rs_recovered", "summary":[{"type":"summary_text", "text":"Recovered reasoning."}], "encrypted_content":"sealed"},
                        {"type":"message", "content":[{"type":"output_text", "text":"recovered"}]}
                    ]))
                ]))
            }
        }).await;
        let c = client(&format!("{url}/v1/responses"));
        c.set_user_sampling(true);
        let mut system = ChatMessage::system("rules with id: system-id");
        system.images.push("data:image/png;base64,SYSTEM".into());
        let mut user = ChatMessage::user("read this id: user-id");
        user.images.push("data:image/png;base64,USER".into());
        let reasoning = json!({"type":"reasoning", "id":"rs_1", "encrypted_content":"sealed reasoning", "summary":[{"text":"read the file first"}]});
        let mut assistant = replay_message(json!([reasoning]));
        assistant.content = "opening file, id: assistant-id".into();
        assistant.images.push("data:image/png;base64,ASSISTANT".into());
        assistant.reasoning = Some("read the file first".into());
        let arguments = r#"{"path":"a.rs","id":"argument-id","nested":{"id":"nested-id","encrypted_content":"tool data"}}"#;
        assistant.tool_calls.push(ToolCall { id: "call_1".into(), name: "read_file".into(), args_json: arguments.into() });
        let result = r#"{"id":"result-id","content":"file contents"}"#;
        let history = vec![system, user, assistant, ChatMessage::tool_result("call_1", result)];
        let schema = json!({"$id":"schema-id", "type":"object", "properties":{"id":{"type":"string"}, "path":{"type":"string"}}});
        let tools = vec![ToolSpec { name: "read_file".into(), description: "Read a file".into(), parameters_json: schema.to_string() }];
        let options = TurnOptions { custom_effort: Some("max".into()), max_tokens: Some(1234), temperature: Some(0.4), top_p: Some(0.8), ..Default::default() };
        let full = body(&c, &history, &tools, &options);
        let events: Vec<_> = c.stream_with_options(&history, &tools, &options).await.unwrap().collect().await;
        assert!(events.iter().any(|e| matches!(e, Ok(LlmEvent::TextDelta(s)) if s == "recovered")));
        let thoughts: String = events.iter().filter_map(|e| match e {
            Ok(LlmEvent::ReasoningDelta(text)) => Some(text.as_str()), _ => None,
        }).collect();
        assert_eq!(thoughts, "Recovered reasoning.", "the fallback must not hide or duplicate readable thoughts");
        assert!(events.iter().any(|e| matches!(e, Ok(LlmEvent::Usage(u)) if u.prompt == Some(50) && u.completion == Some(9) && u.cached == Some(40))));
        assert!(events.iter().all(Result::is_ok));
        let sent: Vec<_> = log.lock().unwrap().iter().map(|r| r.json()).collect();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0], full);
        let mut visible = full.clone();
        visible["input"] = json!([
            {"role":"system", "content":[{"type":"input_text", "text":"rules with id: system-id"}, {"type":"input_image", "image_url":"data:image/png;base64,SYSTEM"}]},
            {"role":"user", "content":[{"type":"input_text", "text":"read this id: user-id"}, {"type":"input_image", "image_url":"data:image/png;base64,USER"}]},
            {"role":"assistant", "content":"Earlier assistant reasoning (visible text):\nread the file first"},
            {"role":"assistant", "content":[{"type":"input_text", "text":"opening file, id: assistant-id"}, {"type":"input_image", "image_url":"data:image/png;base64,ASSISTANT"}]},
            {"type":"function_call", "call_id":"call_1", "name":"read_file", "arguments":arguments},
            {"type":"function_call_output", "call_id":"call_1", "output":result}
        ]);
        assert_eq!(sent[1], visible, "only opaque replay may give way");
        assert_eq!(sent[1].to_string().matches("read the file first").count(), 1);
        assert_eq!(sent[1]["store"], false);
        assert_eq!(sent[1]["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(sent[1]["reasoning"], json!({"effort":"max", "summary":"auto"}));
        assert_eq!(sent[1]["max_output_tokens"], 1234);
        assert_eq!(sent[1]["tools"][0]["parameters"], schema);
        assert_eq!(history[2].replay.as_ref().unwrap()["reasoning"][0], reasoning);
        assert_eq!(history[2].reasoning.as_deref(), Some("read the file first"));
        assert!(c.learned.read().responses_visible_only);

        let events: Vec<_> = c.stream_with_options(&history, &tools, &options).await.unwrap().collect().await;
        assert!(events.iter().all(Result::is_ok));
        assert_eq!(log.lock().unwrap().len(), 3, "the next turn starts in the accepted mode");
        assert_eq!(log.lock().unwrap()[2].json(), visible);
        c.set_model(c.model());
        c.set_endpoint(c.endpoint());
        assert!(c.learned.read().responses_visible_only, "no-op setters keep learned behavior");
        c.set_model("another-model");
        assert!(!c.learned.read().responses_visible_only);
        let events: Vec<_> = c.stream_with_options(&history, &tools, &options).await.unwrap().collect().await;
        assert!(events.iter().all(Result::is_ok));
        assert_eq!(log.lock().unwrap().len(), 5);
        assert_eq!(log.lock().unwrap()[3].json()["input"], full["input"]);
        c.set_endpoint(Endpoint::new(ApiProtocol::OpenAi, format!("{url}/v1/responses"), Some("new-key".into())));
        assert!(!c.learned.read().responses_visible_only);
        let events: Vec<_> = c.stream_with_options(&history, &tools, &options).await.unwrap().collect().await;
        assert!(events.iter().all(Result::is_ok));
        assert_eq!(log.lock().unwrap().len(), 7);
        assert_eq!(log.lock().unwrap()[5].json()["input"], full["input"]);
    }

    #[tokio::test]
    async fn visible_replay_keeps_sampling_fields_already_dropped_on_this_request() {
        let (url, log) = crate::openai::test_server::serve(|req| {
            let sent = req.json();
            if sent.get("temperature").is_some() {
                ("400 Bad Request", "application/json", "temperature is unsupported".into())
            } else if sent.get("top_p").is_some() {
                ("422 Unprocessable Entity", "application/json", "top_p is not supported".into())
            } else if has_bound_reasoning(&sent) {
                ("400 Bad Request", "application/json", resource_error())
            } else {
                ("200 OK", "text/event-stream", sse(&[text_reply("ok")]))
            }
        }).await;
        let c = client(&format!("{url}/v1/responses"));
        c.set_user_sampling(true);
        let history = vec![replay_message(json!([{"type":"reasoning", "id":"rs_1", "summary":[{"text":"saved summary"}]}]))];
        let options = TurnOptions { temperature: Some(0.4), top_p: Some(0.8), max_tokens: Some(100), custom_effort: Some("high".into()), ..Default::default() };
        let events: Vec<_> = c.stream_with_options(&history, &[], &options).await.unwrap().collect().await;
        assert!(events.iter().all(Result::is_ok));
        let sent: Vec<_> = log.lock().unwrap().iter().map(|r| r.json()).collect();
        assert_eq!(sent.len(), 4);
        assert!(sent[0].get("temperature").is_some());
        assert!(sent[1].get("temperature").is_none() && sent[1].get("top_p").is_some());
        assert!(has_bound_reasoning(&sent[2]));
        let mut expected = sent[2].clone();
        expected["input"] = json!([
            {"role":"assistant", "content":"Earlier assistant reasoning (visible text):\nsaved summary"},
            {"role":"assistant", "content":"previous answer"}
        ]);
        assert_eq!(sent[3], expected);
        assert!(sent[3].get("temperature").is_none() && sent[3].get("top_p").is_none());
        assert_eq!(c.learned.read().responses_dropped, ["temperature", "top_p"]);
        let events: Vec<_> = c.stream_with_options(&history, &[], &options).await.unwrap().collect().await;
        assert!(events.iter().all(Result::is_ok));
        assert_eq!(log.lock().unwrap().len(), 5);
        assert_eq!(log.lock().unwrap()[4].json(), expected);
        c.set_model("other-model");
        assert!(c.learned.read().responses_dropped.is_empty());
        assert!(!c.learned.read().responses_visible_only);
    }

    #[tokio::test]
    async fn responses_tool_calls_round_trip_with_stateless_replay() {
        let reasoning = json!({"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":"sealed"});
        let expected = reasoning.clone();
        let (url, log) = crate::openai::test_server::serve(move |req| {
            assert_eq!(req.path, "/v1/responses");
            let input = req.json()["input"].as_array().unwrap().clone();
            let output = if input.iter().any(|v| v["type"] == "function_call_output") {
                assert!(input.iter().any(|v| v == &expected), "reasoning replay was lost");
                assert!(input.iter().any(|v| v["type"] == "function_call" && v["call_id"] == "call_1"));
                assert!(input.iter().any(|v| v["type"] == "function_call_output" && v["call_id"] == "call_1" && v["output"] == "contents"));
                text_reply("done")
            } else {
                completed(json!([expected,{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read_file","arguments":"{\"path\":\"a.rs\"}"}]))
            };
            ("200 OK", "text/event-stream", sse(&[output]))
        }).await;
        let c = client(&format!("{url}/v1/responses"));
        let mut history = vec![ChatMessage::system("rules"), ChatMessage::user("read")];
        let first: Vec<_> = c.stream(&history, &[]).await.unwrap().collect().await;
        let mut assistant = ChatMessage::assistant("");
        for event in first {
            match event.unwrap() {
                LlmEvent::Replay(replay) => assistant.replay = Some(replay),
                LlmEvent::ToolCallDelta { id: Some(id), name: Some(name), args_delta, .. } => {
                    assistant.tool_calls.push(ToolCall { id, name, args_json: args_delta });
                }
                _ => {}
            }
        }
        history.push(assistant);
        history.push(ChatMessage::tool_result("call_1", "contents"));
        let second: Vec<_> = c.stream(&history, &[]).await.unwrap().collect().await;
        assert!(second.iter().any(|e| matches!(e, Ok(LlmEvent::TextDelta(s)) if s == "done")));
        assert_eq!(log.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn replay_recovery_keeps_its_request_and_cannot_teach_a_switched_scope() {
        use std::sync::{Arc, Mutex};
        use std::sync::atomic::{AtomicUsize, Ordering};
        for (switch_endpoint, switch_back) in [(false, false), (true, false), (false, true), (true, true)] {
            let shared = Arc::new(Mutex::new(None::<Arc<Client>>));
            let active = shared.clone();
            let count = AtomicUsize::new(0);
            let (url, log) = crate::openai::test_server::serve(move |req| {
                assert_eq!(req.path, "/v1/responses", "a retry changed its endpoint");
                assert_eq!(req.json()["model"], "gpt-6.1-sol", "a retry changed its model");
                match count.fetch_add(1, Ordering::Relaxed) {
                    0 => ("400 Bad Request", "application/json", "temperature is unsupported".into()),
                    1 => {
                        let c = active.lock().unwrap().as_ref().unwrap().clone();
                        if switch_endpoint {
                            let old = c.endpoint();
                            c.set_endpoint(Endpoint::new(ApiProtocol::OpenAi, format!("{}/other/responses", c.base_url()), None));
                            if switch_back { c.set_endpoint(old); }
                        } else {
                            c.set_model("other-model");
                            if switch_back { c.set_model("gpt-6.1-sol"); }
                        }
                        c.set_user_sampling(false);
                        let mut learned = c.learned.write();
                        learned.responses = true;
                        learned.responses_dropped.push("top_p");
                        ("400 Bad Request", "application/json", resource_error())
                    }
                    2 => ("200 OK", "text/event-stream", sse(&[text_reply("ok")])),
                    _ => panic!("too many attempts"),
                }
            }).await;
            let c = Arc::new(client(&format!("{url}/v1/responses")));
            *shared.lock().unwrap() = Some(c.clone());
            c.set_user_sampling(true);
            let history = vec![replay_message(json!([{"type":"reasoning", "id":"rs_1", "encrypted_content":"sealed"}]))];
            let options = TurnOptions { temperature: Some(0.4), top_p: Some(0.8), custom_effort: Some("max".into()), max_tokens: Some(100), ..Default::default() };
            let events: Vec<_> = c.stream_with_options(&history, &[], &options).await.unwrap().collect().await;
            assert!(events.iter().all(Result::is_ok));
            let sent: Vec<_> = log.lock().unwrap().iter().map(|r| r.json()).collect();
            assert_eq!(sent.len(), 3);
            assert!(has_bound_reasoning(&sent[1]));
            let mut expected = sent[1].clone();
            expected["input"] = json!([{"role":"assistant", "content":"previous answer"}]);
            assert_eq!(sent[2], expected, "sampling and effort must not be re-resolved");
            assert!(sent[2].get("temperature").is_none());
            assert!(sent[2].get("top_p").is_some());
            let learned = c.learned.read();
            assert!(!learned.responses_visible_only, "the stale response taught a different scope");
            assert_eq!(learned.responses_dropped, ["top_p"], "old sampling knowledge leaked through the reset");
            assert!(learned.responses, "new scope knowledge was overwritten");
        }
    }

    #[tokio::test]
    async fn a_refused_visible_fallback_is_returned_without_another_repair() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        for refusal in [resource_error(), "reasoning.summary is unsupported".into(), "temperature is unsupported".into()] {
            let count = AtomicUsize::new(0);
            let reply = refusal.clone();
            let (url, log) = crate::openai::test_server::serve(move |_| {
                let error = if count.fetch_add(1, Ordering::Relaxed) == 0 { resource_error() } else { reply.clone() };
                ("400 Bad Request", "application/json", error)
            }).await;
            let c = client(&format!("{url}/v1/responses"));
            c.set_max_retries(3);
            c.set_user_sampling(true);
            let history = vec![replay_message(json!([{"type":"reasoning", "id":"rs_1", "encrypted_content":"sealed"}]))];
            let options = TurnOptions { temperature: Some(0.4), ..Default::default() };
            let result = c.stream_with_options(&history, &[], &options).await;
            assert!(matches!(result, Err(LlmError::Status { status: 400, body }) if body == refusal));
            let sent: Vec<_> = log.lock().unwrap().iter().map(|r| r.json()).collect();
            assert_eq!(sent.len(), 2);
            assert!(has_bound_reasoning(&sent[0]));
            assert!(!has_bound_reasoning(&sent[1]));
            assert!(!c.learned.read().responses_visible_only, "a refusal teaches no successful replay mode");
            assert!(c.learned.read().responses_dropped.is_empty());
        }
    }

    #[tokio::test]
    async fn no_bound_replay_or_an_unrelated_error_never_retries() {
        let bound = json!([{"type":"reasoning", "id":"rs_1", "encrypted_content":"sealed"}]);
        let mut other_protocol = replay_message(bound.clone());
        other_protocol.replay.as_mut().unwrap()["protocol"] = json!("anthropic");
        for (status, error, message) in [
            ("400 Bad Request", RESOURCE_MISMATCH, ChatMessage::user("id: ordinary text")),
            ("400 Bad Request", RESOURCE_MISMATCH, replay_message(json!([{"type":"reasoning", "summary":[{"text":"plain"}]}]))),
            ("400 Bad Request", RESOURCE_MISMATCH, replay_message(json!([{"type":"reasoning", "id":null, "encrypted_content":""}]))),
            ("400 Bad Request", RESOURCE_MISMATCH, other_protocol),
            ("400 Bad Request", "bad request", replay_message(bound.clone())),
            ("400 Bad Request", "context length exceeded", replay_message(bound.clone())),
            ("400 Bad Request", "invalid tool schema: id is required", replay_message(bound.clone())),
            ("400 Bad Request", "reasoning.summary is not supported", replay_message(bound.clone())),
            ("401 Unauthorized", RESOURCE_MISMATCH, replay_message(bound.clone())),
            ("403 Forbidden", "invalid API key", replay_message(bound.clone())),
            ("422 Unprocessable Entity", RESOURCE_MISMATCH, replay_message(bound)),
        ] {
            let (url, log) = crate::openai::test_server::serve(move |_| (
                status, "application/json", json!({"error":{"message":error}}).to_string(),
            )).await;
            let c = client(&format!("{url}/v1/responses"));
            c.set_max_retries(3);
            let result = c.stream(&[message], &[]).await;
            assert!(matches!(result, Err(LlmError::Status { .. })), "{status}: {error}");
            assert_eq!(log.lock().unwrap().len(), 1, "{status}: {error}");
            assert!(!c.learned.read().responses_visible_only);
        }
    }

    #[tokio::test]
    async fn encrypted_only_recovery_never_invents_readable_thoughts() {
        for item in [
            json!({"type":"reasoning", "encrypted_content":"sealed", "summary":[]}),
            json!({"type":"reasoning", "id":"rs_1", "summary":[]}),
        ] {
            let output = item.clone();
            let (url, log) = crate::openai::test_server::serve(move |req| {
                if has_bound_reasoning(&req.json()) {
                    ("400 Bad Request", "application/json", resource_error())
                } else {
                    ("200 OK", "text/event-stream", sse(&[completed(json!([
                        output, {"type":"message", "content":[{"type":"output_text", "text":"ok"}]}
                    ]))]))
                }
            }).await;
            let c = client(&format!("{url}/v1/responses"));
            let history = vec![replay_message(json!([item]))];
            let events: Vec<_> = c.stream(&history, &[]).await.unwrap().collect().await;
            assert!(events.iter().all(Result::is_ok));
            assert!(!events.iter().any(|e| matches!(e, Ok(LlmEvent::ReasoningDelta(_)))));
            assert!(events.iter().any(|e| matches!(e, Ok(LlmEvent::Replay(_)))));
            assert_eq!(log.lock().unwrap().len(), 2);
            assert_eq!(log.lock().unwrap()[1].json()["input"], json!([{"role":"assistant", "content":"previous answer"}]));
        }
    }

    #[tokio::test]
    async fn an_azure_error_inside_an_accepted_stream_is_never_resubmitted() {
        for prefix in [
            vec![],
            vec![json!({"type":"response.output_text.delta", "output_index":0, "content_index":0, "delta":"partial"})],
            vec![
                json!({"type":"response.output_item.added", "output_index":0, "item":{"type":"function_call", "call_id":"c", "name":"read_file", "arguments":""}}),
                json!({"type":"response.function_call_arguments.delta", "output_index":0, "delta":"{\"path\""}),
            ],
        ] {
            let has_tool = prefix.iter().any(|v| v["type"] == "response.function_call_arguments.delta");
            let mut frames = prefix;
            frames.push(json!({"type":"response.failed", "response":{"error":{"message":RESOURCE_MISMATCH}}}));
            let (url, log) = crate::openai::test_server::serve(move |_| (
                "200 OK", "text/event-stream", sse(&frames),
            )).await;
            let c = client(&format!("{url}/v1/responses"));
            c.set_max_retries(3);
            let history = vec![replay_message(json!([{"type":"reasoning", "id":"rs_1", "encrypted_content":"sealed"}]))];
            let events: Vec<_> = c.stream(&history, &[]).await.unwrap().collect().await;
            assert!(events.iter().any(Result::is_err));
            assert_eq!(events.iter().any(|e| matches!(e, Ok(LlmEvent::ToolCallDelta { .. }))), has_tool);
            assert!(!events.iter().any(|e| matches!(e, Ok(LlmEvent::Done(_)))));
            assert_eq!(log.lock().unwrap().len(), 1);
            assert!(!c.learned.read().responses_visible_only);
        }
    }

    #[tokio::test]
    async fn a_stream_cut_after_a_tool_delta_is_never_retried() {
        let (url, log)=crate::openai::test_server::serve(|_| (
            "200 OK", "text/event-stream", sse(&[
                json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"c","name":"read_file","arguments":""}}),
                json!({"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"path\""}),
            ]),
        )).await;
        let c=client(&format!("{url}/v1/responses"));
        c.set_max_retries(3);
        let history = vec![ChatMessage::user("read"), replay_message(json!([{"type":"reasoning", "id":"rs_1", "encrypted_content":"sealed"}]))];
        let events:Vec<_>=c.stream(&history,&[]).await.unwrap().collect().await;
        assert!(matches!(events.last(),Some(Err(LlmError::Stream(s))) if s==crate::client::CLOSED_EARLY));
        assert!(!events.iter().any(|e|matches!(e,Ok(LlmEvent::Done(_)))));
        assert_eq!(log.lock().unwrap().len(),1);
    }
}
