//! A stand-in for LM Studio: one loaded model, and scripted answers.
//!
//! Only the agent's own turns — requests that offer tools — take the next
//! scripted reply. Everything else the app asks the model on the side (the
//! recap after a turn, a probe) gets a short plain answer, so a scenario
//! scripts the conversation and not the app's housekeeping.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const MODEL: &str = "mock-model";

/// What the model says in one turn.
#[derive(Clone, Debug)]
pub enum Reply {
    /// Plain text, streamed a word at a time.
    Text(String),
    /// A call to one tool with these arguments.
    ToolCall { name: String, arguments: serde_json::Value },
    /// Text that takes `per_word` per word to arrive, for a turn that is
    /// still running when the scenario acts.
    Slow { text: String, per_word: Duration },
    /// Text the server cuts off at its output limit (`finish_reason: length`).
    Cut(String),
}

/// One request the app made.
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub body: serde_json::Value,
    pub at: Instant,
}

impl Request {
    /// A turn of the agent, as opposed to a side request.
    pub fn is_turn(&self) -> bool {
        self.method == "POST" && self.body["tools"].as_array().is_some_and(|t| !t.is_empty())
    }
}

pub struct MockServer {
    /// Base URL as the app is configured with it, ending in `/v1`.
    pub url: String,
    requests: Arc<Mutex<Vec<Request>>>,
    replies: Arc<Mutex<VecDeque<Reply>>>,
    /// Extra entries appended to `/api/v0/models`'s listing, beyond `MODEL`.
    v0_extra_models: Arc<Mutex<Vec<serde_json::Value>>>,
    /// Body for `/api/v1/models`, or `None` to answer it 404 (the default —
    /// most scenarios have no reason to care about LM Studio's v1 listing,
    /// only its v0 one).
    v1_models: Arc<Mutex<Option<serde_json::Value>>>,
}

impl MockServer {
    pub fn start(script: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a local port");
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let replies = Arc::new(Mutex::new(VecDeque::from(script)));
        let v0_extra_models = Arc::new(Mutex::new(Vec::new()));
        let v1_models = Arc::new(Mutex::new(None));
        let (req2, rep2, v0e2, v12) = (requests.clone(), replies.clone(), v0_extra_models.clone(), v1_models.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (req3, rep3, v0e3, v13) = (req2.clone(), rep2.clone(), v0e2.clone(), v12.clone());
                std::thread::spawn(move || {
                    let _ = serve(stream, &req3, &rep3, &v0e3, &v13);
                });
            }
        });
        MockServer { url, requests, replies, v0_extra_models, v1_models }
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    pub fn turns(&self) -> Vec<Request> {
        self.requests().into_iter().filter(Request::is_turn).collect()
    }

    /// Scripted replies not yet asked for.
    pub fn replies_left(&self) -> usize {
        self.replies.lock().unwrap().len()
    }

    /// Make `/api/v1/models` answer with `MODEL` reporting these reasoning
    /// presets, in LM Studio's native `capabilities.reasoning` shape — for
    /// scenarios about a model whose reasoning settings the server actually
    /// lists (as opposed to the plain `/api/v0/models` entry `start` already
    /// serves for it, which names none).
    pub fn report_reasoning(&self, presets: &[&str], default: &str) {
        *self.v1_models.lock().unwrap() = Some(serde_json::json!({ "models": [ {
            "key": MODEL,
            "capabilities": {
                "trained_for_tool_use": true,
                "reasoning": { "allowed_options": presets, "default": default }
            },
            "loaded_instances": [ { "config": { "context_length": 32768 } } ]
        } ] }));
    }

    /// Add a model to `/api/v0/models`'s listing that carries no capability
    /// information at all — the server saying nothing about whether it can
    /// reason, as opposed to saying it cannot. Turns run against it are still
    /// answered from the same script as `MODEL`'s.
    pub fn add_model_the_server_says_nothing_about(&self, id: &str) {
        self.v0_extra_models.lock().unwrap().push(serde_json::json!({
            "id": id, "object": "model", "type": "llm", "state": "loaded",
            "max_context_length": 32768, "loaded_context_length": 32768
        }));
    }
}

fn serve(
    stream: TcpStream,
    requests: &Mutex<Vec<Request>>,
    replies: &Mutex<VecDeque<Reply>>,
    v0_extra_models: &Mutex<Vec<serde_json::Value>>,
    v1_models: &Mutex<Option<serde_json::Value>>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header == "\r\n" {
            break;
        }
        if let Some((k, v)) = header.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                length = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut raw = vec![0u8; length];
    reader.read_exact(&mut raw)?;
    let body: serde_json::Value = serde_json::from_slice(&raw).unwrap_or(serde_json::Value::Null);
    let request = Request { method: method.clone(), path: path.clone(), body, at: Instant::now() };
    requests.lock().unwrap().push(request.clone());

    let mut out = stream;
    if method == "GET" {
        return if path.ends_with("/api/v1/models") {
            match v1_models.lock().unwrap().clone() {
                Some(body) => json(&mut out, &body),
                None => out.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"),
            }
        } else if path.ends_with("/api/v0/models") {
            let mut data = vec![serde_json::json!({
                "id": MODEL, "object": "model", "type": "llm", "state": "loaded",
                "max_context_length": 32768, "loaded_context_length": 32768
            })];
            data.extend(v0_extra_models.lock().unwrap().iter().cloned());
            json(&mut out, &serde_json::json!({ "data": data }))
        } else if path.ends_with("/v1/models") && !path.contains("/api/") {
            json(&mut out, &serde_json::json!({ "object": "list", "data": [ { "id": MODEL, "object": "model" } ] }))
        } else {
            out.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
        };
    }

    let reply = if request.is_turn() {
        replies.lock().unwrap().pop_front().unwrap_or_else(|| Reply::Text("(the script has no more replies)".into()))
    } else {
        Reply::Text("ok".into())
    };

    if request.body["stream"] != serde_json::Value::Bool(true) {
        let text = match &reply {
            Reply::Text(t) | Reply::Slow { text: t, .. } | Reply::Cut(t) => t.clone(),
            Reply::ToolCall { .. } => String::new(),
        };
        return json(&mut out, &serde_json::json!({
            "choices": [ { "message": { "role": "assistant", "content": text }, "finish_reason": "stop" } ],
            "usage": { "prompt_tokens": 10, "completion_tokens": 2 }
        }));
    }

    out.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n")?;
    let chunk = |out: &mut TcpStream, v: serde_json::Value| out.write_all(format!("data: {v}\n\n").as_bytes());
    match reply {
        Reply::Text(text) => {
            for word in words(&text) {
                chunk(&mut out, serde_json::json!({ "choices": [ { "delta": { "content": word } } ] }))?;
            }
            chunk(&mut out, serde_json::json!({ "choices": [ { "delta": {}, "finish_reason": "stop" } ] }))?;
        }
        Reply::Slow { text, per_word } => {
            for word in words(&text) {
                chunk(&mut out, serde_json::json!({ "choices": [ { "delta": { "content": word } } ] }))?;
                out.flush()?;
                std::thread::sleep(per_word);
            }
            chunk(&mut out, serde_json::json!({ "choices": [ { "delta": {}, "finish_reason": "stop" } ] }))?;
        }
        Reply::Cut(text) => {
            for word in words(&text) {
                chunk(&mut out, serde_json::json!({ "choices": [ { "delta": { "content": word } } ] }))?;
            }
            chunk(&mut out, serde_json::json!({ "choices": [ { "delta": {}, "finish_reason": "length" } ] }))?;
        }
        Reply::ToolCall { name, arguments } => {
            chunk(&mut out, serde_json::json!({ "choices": [ { "delta": { "tool_calls": [ {
                "index": 0, "id": "call_1", "type": "function",
                "function": { "name": name, "arguments": arguments.to_string() }
            } ] } } ] }))?;
            chunk(&mut out, serde_json::json!({ "choices": [ { "delta": {}, "finish_reason": "tool_calls" } ] }))?;
        }
    }
    out.write_all(b"data: [DONE]\n\n")?;
    out.flush()
}

/// Words with the space that follows each, so the streamed text adds up.
fn words(text: &str) -> Vec<String> {
    text.split_inclusive(' ').map(str::to_string).collect()
}

fn json(out: &mut TcpStream, v: &serde_json::Value) -> std::io::Result<()> {
    let body = v.to_string();
    out.write_all(
        format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
            .as_bytes(),
    )
}
