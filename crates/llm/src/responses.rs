//! OpenAI Responses for models whose gateway refuses Chat Completions. The
//! conversation stays local: every request carries its own input and signed
//! reasoning, never a server-side conversation id.

use std::collections::{BTreeMap, HashSet};

use serde_json::{json, Value};

use crate::client::{pump, Client, EventStream, Malformed, WireDecoder};
use crate::parse::SseDecoder;
use crate::types::{ChatMessage, CostDetails, FinishReason, LlmError, LlmEvent, Role, ToolSpec, TurnOptions, Usage};

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
            return Ok(pump(response, busy, Decoder::default()));
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
    sse: SseDecoder,
    whole: Option<bool>,
    reply: Vec<u8>,
    calls: BTreeMap<usize, Call>,
    texts: HashSet<(usize, usize)>,
    thoughts: HashSet<(usize, usize)>,
    reasoning: BTreeMap<usize, Value>,
    terminal: bool,
    failed: bool,
    malformed: Malformed,
}

impl Decoder {
    fn call(&mut self, index: usize, item: &Value, out: &mut Vec<Result<LlmEvent, LlmError>>) {
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

    fn item(&mut self, index: usize, item: &Value, out: &mut Vec<Result<LlmEvent, LlmError>>) {
        match item["type"].as_str() {
            Some("function_call") => self.call(index, item, out),
            Some("reasoning") => {
                if let Some(summary) = item["summary"].as_array() {
                    for (part, text) in summary.iter().enumerate() {
                        if let Some(text) = text["text"].as_str().filter(|s| !s.is_empty()) {
                            if self.thoughts.insert((index, part)) { out.push(Ok(LlmEvent::ReasoningDelta(text.into()))); }
                        }
                    }
                }
                self.reasoning.insert(index, item.clone());
            }
            Some("message") => {
                if let Some(content) = item["content"].as_array() {
                    for (part, text) in content.iter().enumerate() {
                        if text["type"] == "refusal" {
                            self.failed = true;
                            out.push(Err(LlmError::Forbidden(text["refusal"].as_str().unwrap_or("The provider withheld the answer").into())));
                        } else if let Some(text) = text["text"].as_str().filter(|s| !s.is_empty()) {
                            if self.texts.insert((index, part)) { out.push(Ok(LlmEvent::TextDelta(text.into()))); }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn complete(&mut self, response: &Value, incomplete: bool, out: &mut Vec<Result<LlmEvent, LlmError>>) {
        if let Some(items) = response["output"].as_array() {
            for (index, item) in items.iter().enumerate() { self.item(index, item, out); }
        };
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
            out.push(Ok(LlmEvent::Usage(Usage {
                prompt: usage["input_tokens"].as_i64(), completion: usage["output_tokens"].as_i64(),
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
        match value["type"].as_str() {
            Some("response.output_text.delta") => {
                self.texts.insert((index, value["content_index"].as_u64().unwrap_or(0) as usize));
                if let Some(text) = value["delta"].as_str() { out.push(Ok(LlmEvent::TextDelta(text.into()))); }
            }
            Some("response.reasoning_summary_text.delta" | "response.reasoning_text.delta") => {
                self.thoughts.insert((index, value["summary_index"].as_u64().unwrap_or(0) as usize));
                if let Some(text) = value["delta"].as_str() { out.push(Ok(LlmEvent::ReasoningDelta(text.into()))); }
            }
            Some("response.output_item.added" | "response.output_item.done") => self.item(index, &value["item"], &mut out),
            Some("response.function_call_arguments.delta") => {
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
