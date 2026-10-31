//! OpenAI Responses for models whose gateway refuses Chat Completions. The
//! conversation stays local: every request carries its own input and signed
//! reasoning, never a server-side conversation id.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::client::{pump, Client, EventStream, Malformed, WireDecoder};
use crate::parse::SseDecoder;
use crate::types::{ChatMessage, CompletionMeasurement, CostDetails, FinishReason, LlmError, LlmEvent, Role, ToolSpec, TurnOptions, Usage};

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

pub(crate) async fn stream(client: &Client, messages: &[ChatMessage], tools: &[ToolSpec], options: &TurnOptions) -> Result<EventStream, LlmError> {
    let busy = client.busy();
    let url = format!("{}/responses", crate::openai::api_base(client));
    let headers = crate::openai::headers(client);
    let mut dropped = client.learned.read().responses_dropped.clone();
    let mut request = body(client, messages, tools, options);
    loop {
        for key in &dropped {
            if let Some(map) = request.as_object_mut() { map.remove(*key); }
        }
        let response = client.post(&url, &headers, &request).await?;
        if response.status().is_success() {
            let mut learned = client.learned.write();
            for key in dropped {
                if !learned.responses_dropped.contains(&key) { learned.responses_dropped.push(key); }
            }
            drop(learned);
            let model = request["model"].as_str().unwrap_or_default().to_string();
            return Ok(pump(response, busy, Decoder { model, ..Default::default() }));
        }
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        let lower = text.to_lowercase();
        // Only named sampling fields can give way. Tool schemas, replay and
        // privacy settings are not optional fallbacks to silently throw out.
        let refused = ["temperature", "top_p"].into_iter().find(|key| {
            request.get(*key).is_some() && !dropped.contains(key) && lower.contains(key)
                && ["unsupported", "not supported", "not allowed", "unknown"].iter().any(|s| lower.contains(s))
        });
        if let Some(key) = refused.filter(|_| matches!(status, 400 | 422)) {
            dropped.push(key);
            continue;
        }
        return Err(LlmError::Status { status, body: text });
    }
}

fn body(client: &Client, messages: &[ChatMessage], tools: &[ToolSpec], options: &TurnOptions) -> Value {
    let mut input = Vec::new();
    for message in messages {
        match message.role {
            Role::Tool => input.push(json!({
                "type": "function_call_output", "call_id": message.tool_call_id, "output": message.content,
            })),
            Role::Assistant => {
                if let Some(replay) = message.replay.as_ref().filter(|r| r["protocol"] == "responses") {
                    if let Some(items) = replay["reasoning"].as_array() { input.extend(items.iter().cloned()); }
                }
                if !message.content.is_empty() {
                    input.push(json!({"role": "assistant", "content": message.content}));
                }
                for call in &message.tool_calls {
                    input.push(json!({"type": "function_call", "call_id": call.id, "name": call.name, "arguments": call.args_json}));
                }
            }
            Role::System | Role::User => {
                let content = if message.images.is_empty() { json!(message.content) } else {
                    let mut parts = Vec::new();
                    if !message.content.is_empty() { parts.push(json!({"type": "input_text", "text": message.content})); }
                    parts.extend(message.images.iter().map(|image| json!({"type": "input_image", "image_url": image})));
                    Value::Array(parts)
                };
                input.push(json!({"role": message.role.as_str(), "content": content}));
            }
        }
    }
    let mut body = json!({
        "model": client.model(), "input": input, "stream": true, "store": false,
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
    if let Some(effort) = client.resolve_effort(messages, options) {
        body["reasoning"] = json!({"effort": if effort == "off" { "none" } else { &effort }, "summary": "auto"});
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
    async fn a_stream_cut_after_a_tool_delta_is_never_retried() {
        let (url, log)=crate::openai::test_server::serve(|_| (
            "200 OK", "text/event-stream", sse(&[
                json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"c","name":"read_file","arguments":""}}),
                json!({"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"path\""}),
            ]),
        )).await;
        let c=client(&format!("{url}/v1/responses"));
        c.set_max_retries(3);
        let events:Vec<_>=c.stream(&[ChatMessage::user("read")],&[]).await.unwrap().collect().await;
        assert!(matches!(events.last(),Some(Err(LlmError::Stream(s))) if s==crate::client::CLOSED_EARLY));
        assert!(!events.iter().any(|e|matches!(e,Ok(LlmEvent::Done(_)))));
        assert_eq!(log.lock().unwrap().len(),1);
    }
}
