//! A stand-in for LM Studio with one loaded model and scripted answers. Only
//! agent turns (requests that offer tools) take the next scripted reply; side
//! requests (recap, probes) get a short plain answer.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const MODEL: &str = "mock-model";

#[derive(Clone, Debug)]
pub enum Reply {
    /// Streamed a word at a time.
    Text(String),
    ToolCall { name: String, arguments: serde_json::Value },
    /// For a turn still running when the scenario acts.
    Slow { text: String, per_word: Duration },
    /// Cut off at the output limit (`finish_reason: length`).
    Cut(String),
}

#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub body: serde_json::Value,
    pub at: Instant,
}

impl Request {
    /// As opposed to a side request.
    pub fn is_turn(&self) -> bool {
        self.method == "POST" && self.body["tools"].as_array().is_some_and(|t| !t.is_empty()) && !self.is_warm_up()
    }

    /// The cache warm-up (`warm.rs`): it asks for one token.
    pub fn is_warm_up(&self) -> bool {
        self.method == "POST" && self.body["max_tokens"] == 1
    }
}

pub struct MockServer {
    /// Ends in `/v1`.
    pub url: String,
    /// The one model it serves, `MODEL` unless started with another.
    pub model: String,
    requests: Arc<Mutex<Vec<Request>>>,
    replies: Arc<Mutex<VecDeque<Reply>>>,
    /// Beyond `MODEL`.
    v0_extra_models: Arc<Mutex<Vec<serde_json::Value>>>,
    /// `None` answers 404, the default: most scenarios only need the v0 listing.
    v1_models: Arc<Mutex<Option<serde_json::Value>>>,
    side: Arc<SideRequests>,
}

#[derive(Default)]
struct SideRequests {
    per_word: Mutex<Option<Duration>>,
    /// Slow side answers the app hung up on.
    dropped: std::sync::atomic::AtomicUsize,
    /// Keyed by a substring of the system prompt.
    answers: Mutex<Vec<(String, String)>>,
    /// Lets a scenario act while the model is still thinking.
    turn_delay: Mutex<Option<Duration>>,
    cloud: std::sync::atomic::AtomicBool,
}

impl MockServer {
    pub fn start(script: Vec<Reply>) -> Self {
        Self::start_with_model(MODEL, script)
    }

    /// A second server in one scenario, told apart by the model it serves.
    pub fn start_with_model(model: &str, script: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a local port");
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let replies = Arc::new(Mutex::new(VecDeque::from(script)));
        let v0_extra_models = Arc::new(Mutex::new(Vec::new()));
        let v1_models = Arc::new(Mutex::new(None));
        let side = Arc::new(SideRequests::default());
        let served = Arc::new(model.to_string());
        let (req2, rep2, v0e2, v12, side2) =
            (requests.clone(), replies.clone(), v0_extra_models.clone(), v1_models.clone(), side.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (req3, rep3, v0e3, v13, side3) = (req2.clone(), rep2.clone(), v0e2.clone(), v12.clone(), side2.clone());
                let served = served.clone();
                std::thread::spawn(move || {
                    let closer = stream.try_clone();
                    let _ = serve(stream, &served, &req3, &rep3, &v0e3, &v13, &side3);
                    // Close gracefully: dropping the socket at once lets Windows send a reset,
                    // which can cut off the end of a reply the app is still reading.
                    if let Ok(mut socket) = closer {
                        let _ = socket.shutdown(std::net::Shutdown::Write);
                        let _ = socket.set_read_timeout(Some(Duration::from_secs(2)));
                        let mut drain = [0u8; 1024];
                        while matches!(socket.read(&mut drain), Ok(n) if n > 0) {}
                    }
                });
            }
        });
        MockServer { url, model: model.to_string(), requests, replies, v0_extra_models, v1_models, side }
    }

    /// Lets the scenario press a key that must land before the answer.
    pub fn delay_turns(&self, delay: Duration) {
        *self.side.turn_delay.lock().unwrap() = Some(delay);
    }

    /// Instead of "ok".
    pub fn answer_side_requests(&self, key: &str, text: &str) {
        self.side.answers.lock().unwrap().push((key.to_string(), text.to_string()));
    }

    /// Side answers (the recap) arrive a word every `per_word`, so a scenario
    /// can act while one is still being written.
    pub fn slow_side_requests(&self, per_word: Duration) {
        *self.side.per_word.lock().unwrap() = Some(per_word);
    }

    /// Only the OpenAI model list, as a cloud API answers: no loaded model.
    pub fn serve_as_cloud(&self) {
        self.side.cloud.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn side_requests_dropped(&self) -> usize {
        self.side.dropped.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    pub fn turns(&self) -> Vec<Request> {
        self.requests().into_iter().filter(Request::is_turn).collect()
    }

    pub fn replies_left(&self) -> usize {
        self.replies.lock().unwrap().len()
    }

    /// In LM Studio's native `capabilities.reasoning` shape, for scenarios where
    /// the server lists the model's reasoning settings.
    pub fn report_reasoning(&self, presets: &[&str], default: &str) {
        *self.v1_models.lock().unwrap() = Some(serde_json::json!({ "models": [ {
            "key": self.model,
            "capabilities": {
                "trained_for_tool_use": true,
                "reasoning": { "allowed_options": presets, "default": default }
            },
            "loaded_instances": [ { "config": { "context_length": 32768 } } ]
        } ] }));
    }

    /// No capability information at all: the server says nothing about reasoning,
    /// as opposed to saying the model cannot. Answered from the same script.
    pub fn add_model_the_server_says_nothing_about(&self, id: &str) {
        self.v0_extra_models.lock().unwrap().push(serde_json::json!({
            "id": id, "object": "model", "type": "llm", "state": "loaded",
            "max_context_length": 32768, "loaded_context_length": 32768
        }));
    }
}

fn serve(
    stream: TcpStream,
    model: &str,
    requests: &Mutex<Vec<Request>>,
    replies: &Mutex<VecDeque<Reply>>,
    v0_extra_models: &Mutex<Vec<serde_json::Value>>,
    v1_models: &Mutex<Option<serde_json::Value>>,
    side: &SideRequests,
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
        } else if path.ends_with("/api/v0/models") && !side.cloud.load(std::sync::atomic::Ordering::SeqCst) {
            let mut data = vec![serde_json::json!({
                "id": model, "object": "model", "type": "llm", "state": "loaded",
                "max_context_length": 32768, "loaded_context_length": 32768
            })];
            data.extend(v0_extra_models.lock().unwrap().iter().cloned());
            json(&mut out, &serde_json::json!({ "data": data }))
        } else if path.ends_with("/v1/models") && !path.contains("/api/") {
            json(&mut out, &serde_json::json!({ "object": "list", "data": [ { "id": model, "object": "model" } ] }))
        } else {
            out.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
        };
    }

    let slow_side = *side.per_word.lock().unwrap();
    if let (false, Some(per_word), true) = (request.is_turn(), slow_side, request.body["stream"] == serde_json::Value::Bool(true)) {
        // A failed write means the app hung up.
        let finished = (|| -> std::io::Result<()> {
            out.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n")?;
            for _ in 0..200 {
                out.write_all(format!("data: {}\n\n", serde_json::json!({ "choices": [ { "delta": { "content": "word " } } ] })).as_bytes())?;
                out.flush()?;
                std::thread::sleep(per_word);
            }
            out.write_all(b"data: [DONE]\n\n")?;
            out.flush()
        })();
        if finished.is_err() {
            side.dropped.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        return Ok(());
    }

    if request.is_turn() {
        if let Some(delay) = *side.turn_delay.lock().unwrap() {
            std::thread::sleep(delay);
        }
    }

    let reply = if request.is_turn() {
        replies.lock().unwrap().pop_front().unwrap_or_else(|| Reply::Text("(the script has no more replies)".into()))
    } else {
        let system = request.body["messages"][0]["content"].as_str().unwrap_or_default().to_string();
        let answer = side.answers.lock().unwrap().iter().find(|(key, _)| system.contains(key.as_str())).map(|(_, t)| t.clone());
        Reply::Text(answer.unwrap_or_else(|| "ok".into()))
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
            // Usage without cache info, as LM Studio reports it.
            chunk(&mut out, serde_json::json!({ "choices": [], "usage": { "prompt_tokens": 1200, "completion_tokens": 4 } }))?;
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

/// Each with its trailing space, so the streamed text adds up.
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
