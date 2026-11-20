//! `flashagent --serve`: the agent without a terminal.
//!
//! One session, one HTTP API. A client (the RX Playa Telegram bot) starts a
//! turn with text and polls the event feed while the loop runs — tool calls,
//! the growing answer, approvals it may answer — and reads the finished text.
//! Everything the TUI builds its turn from is reused here: the same backend,
//! permission layer, tools, memory and session files. No terminal is touched:
//! this mode runs on a machine nobody sits at.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use flashagent_llm::ChatMessage;

use super::{build_turn_options, trace};

/// A pending approval: the loop is holding a call until someone answers.
struct PendingApproval {
    respond: tokio::sync::oneshot::Sender<flashagent_core::permissions::Decision>,
}

/// What the HTTP API reads. One guard over everything the bot polls.
#[derive(Default)]
struct ServeState {
    running: bool,
    events: Vec<serde_json::Value>,
    /// The turn's answer so far: every `TurnDelta` in order.
    text: String,
    pending: Option<serde_json::Value>,
    /// The history the last finished turn left behind; the next turn starts
    /// from it, which is what makes one process a continuing session.
    history: Option<Vec<ChatMessage>>,
    last_error: Option<String>,
    turns_done: u64,
}

type SharedState = Arc<Mutex<ServeState>>;

/// The approval gate for a session nobody sits at: the request goes into the
/// event feed, the client answers through `POST /approve`, and a call that
/// waits past the clock is denied — the model adapts to a refusal, a hung
/// start-up it cannot adapt to.
struct ServeGate {
    state: SharedState,
    responders: Mutex<Vec<PendingApproval>>,
    counter: AtomicU64,
    timeout: Duration,
}

impl ServeGate {
    fn push(
        &self,
        id: u64,
        tool: &str,
        args_json: &str,
        diff: Option<String>,
        respond: tokio::sync::oneshot::Sender<flashagent_core::permissions::Decision>,
    ) {
        self.responders.lock().expect("gate responders").push(PendingApproval { respond });
        let mut st = self.state.lock().expect("serve state");
        st.pending = Some(serde_json::json!({
            "type": "approval",
            "id": id,
            "tool": tool,
            "args_json": serde_json::from_str::<serde_json::Value>(args_json)
                .ok()
                .unwrap_or_default(),
            "diff": diff,
        }));
    }

    fn answer(&self, allow: bool) -> bool {
        use flashagent_core::permissions::Decision;
        let pending = self.responders.lock().expect("gate responders").pop();
        if let Some(PendingApproval { respond }) = pending {
            let _ = respond.send(if allow { Decision::Allow } else { Decision::Deny });
            let mut st = self.state.lock().expect("serve state");
            st.pending = None;
            st.events.push(serde_json::json!({"type": "approval_answered", "allow": allow}));
            true
        } else {
            false
        }
    }
}

#[async_trait::async_trait]
impl flashagent_core::permissions::ApprovalGate for ServeGate {
    async fn approve(&self, req: &flashagent_core::permissions::ApprovalRequest) -> flashagent_core::permissions::Decision {
        use flashagent_core::permissions::Decision;
        let id = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.push(id, &req.tool, &req.args_json, req.diff.clone(), tx);
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(decision)) => decision,
            _ => {
                // The answer never came; the feed says the wait is over.
                let mut st = self.state.lock().expect("serve state");
                st.pending = None;
                st.events.push(serde_json::json!({"type": "approval_timeout", "id": id, "tool": req.tool}));
                Decision::Deny
            }
        }
    }
}

/// The map from a loop event to what the client polls.
fn event_to_json(event: &flashagent_core::LoopEvent) -> serde_json::Value {
    use flashagent_core::LoopEvent;
    match event {
        LoopEvent::TurnDelta(text) => serde_json::json!({"type": "text", "text": text}),
        LoopEvent::ReasoningDelta(_) => serde_json::json!({"type": "thinking"}),
        LoopEvent::ToolStarted { name, args_json, .. } => serde_json::json!({
            "type": "tool_start",
            "name": name,
            "args": serde_json::from_str::<serde_json::Value>(args_json)
                .ok()
                .map(|v| {
                    v.as_object()
                        .map(|o| {
                            o.iter()
                                .map(|(k, v)| {
                                    let shown = v
                                        .as_str()
                                        .map(str::to_string)
                                        .unwrap_or_else(|| v.to_string());
                                    (k.clone(), serde_json::Value::String(shown.chars().take(120).collect::<String>()))
                                })
                                .collect::<serde_json::Map<String, serde_json::Value>>()
                        })
                        .unwrap_or_default()
                })
                .unwrap_or_default(),
        }),
        LoopEvent::ToolFinished { is_error, result_len, .. } => {
            serde_json::json!({"type": "tool_end", "ok": !is_error, "result_len": result_len})
        }
        LoopEvent::Usage(u) => serde_json::json!({
            "type": "usage",
            "completion": u.completion.unwrap_or(0),
            "prompt": u.prompt.unwrap_or(0),
        }),
        LoopEvent::CompactionStarted => serde_json::json!({"type": "compaction_started"}),
        LoopEvent::Compacted { saved } => serde_json::json!({"type": "compacted", "saved": saved}),
        LoopEvent::SteeringInjected(text) => serde_json::json!({"type": "steer", "text": text}),
        LoopEvent::StepStarted { step, max_steps } => {
            serde_json::json!({"type": "step", "step": step, "max_steps": max_steps})
        }
        LoopEvent::Done(reason) => serde_json::json!({"type": "done", "reason": format!("{reason:?}")}),
    }
}

/// The headless session: history, tools and the loop, behind the HTTP API.
struct ServeApp {
    state: SharedState,
    source: Arc<crate::backend::BackendSource>,
    perm: &'static flashagent_core::permissions::PermissionedTools,
    cancel: Arc<AtomicBool>,
    session_id: String,
    cwd_display: String,
    config: flashagent_core::AppConfig,
    model: String,
    effort: String,
    /// The system prompt, built once at start-up: memory, outline, identity.
    /// The turn's history starts with it, like the TUI's does.
    system_prompt: String,
}

impl ServeApp {
    fn start_turn(&mut self, text: &str) -> Result<(), String> {
        {
            let st = self.state.lock().expect("serve state");
            if st.running {
                return Err("a turn is already running".into());
            }
        }
        if text.trim().is_empty() {
            return Err("empty prompt".into());
        }
        self.cancel.store(false, Ordering::Relaxed);
        let mut history = self.state.lock().expect("serve state").history.take().unwrap_or_default();
        if history.first().is_none_or(|m| m.role != flashagent_llm::Role::System) {
            history.insert(0, ChatMessage::system(self.system_prompt.clone()));
        }
        history.push(ChatMessage::user(text.to_string()));
        {
            let mut st = self.state.lock().expect("serve state");
            st.running = true;
            st.text.clear();
            st.events.push(serde_json::json!({"type": "turn_started"}));
        }
        let turn_opts = build_turn_options(&self.config, &self.effort);
        let (steer_tx, steer_rx) = tokio::sync::mpsc::unbounded_channel();
        drop(steer_tx); // no steering in serve mode; the loop sees a closed channel
        let state = self.state.clone();
        let source = self.source.clone();
        let cancel = self.cancel.clone();
        let shared = flashagent_core::loop_::SharedHistory::new(history.clone());
        let session_id = self.session_id.clone();
        let cwd_display = self.cwd_display.clone();
        let model = self.model.clone();
        let perm = self.perm;
        tokio::spawn(async move {
            let loop_config =
                flashagent_core::loop_::LoopConfig { base_turn_options: turn_opts, ..Default::default() };
            let loop_ =
                flashagent_core::loop_::AgentLoop::with_steering(loop_config, cancel, steer_rx);
            let state_for_events = state.clone();
            let mut on_event = |event: flashagent_core::LoopEvent| {
                let mut st = state_for_events.lock().expect("serve state");
                if let flashagent_core::LoopEvent::TurnDelta(text) = &event {
                    st.text.push_str(text);
                }
                st.events.push(event_to_json(&event));
            };
            let res = loop_.run(source.as_ref(), perm, shared, &mut on_event).await;
            let mut st = state.lock().expect("serve state");
            st.running = false;
            st.turns_done += 1;
            match res {
                Ok((shared, reason)) => {
                    let snapshot = shared.snapshot();
                    st.history = Some(snapshot.clone());
                    st.events.push(serde_json::json!({"type": "turn_finished", "reason": format!("{reason:?}")}));
                    if let Err(why) = crate::sessions::save_session_file(
                        &session_id,
                        &model,
                        &cwd_display,
                        &snapshot,
                        &[],
                        None,
                        None,
                    ) {
                        st.last_error = Some(why);
                    }
                }
                Err(err) => {
                    let snapshot = err.into_history();
                    st.history = Some(snapshot.clone());
                    st.last_error = Some("the turn failed".into());
                    st.events.push(serde_json::json!({"type": "turn_failed"}));
                    let _ = crate::sessions::save_session_file(
                        &session_id,
                        &model,
                        &cwd_display,
                        &snapshot,
                        &[],
                        None,
                        None,
                    );
                }
            }
        });
        Ok(())
    }

    fn reset(&mut self) -> bool {
        let running = self.state.lock().expect("serve state").running;
        if running {
            return false;
        }
        self.session_id = crate::sessions::new_session_id();
        self.state.lock().expect("serve state").history = None;
        true
    }
}

/// The token every request but `/health` must carry, when one is set.
fn authorized(token: &Option<String>, headers: &[(String, String)]) -> bool {
    match token {
        None => true,
        Some(expected) => headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("authorization")
                && value.strip_prefix("Bearer ") == Some(expected.as_str())
        }),
    }
}

/// One request, read whole: method, path with its query, body. The bot is the
/// only client, so no chunked bodies, no keep-alive games — one request per
/// connection, the answer ends the connection.
struct HttpRequest {
    method: String,
    path: String,
    query: String,
    body: Vec<u8>,
    headers: Vec<(String, String)>,
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> Result<HttpRequest, String> {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end;
    loop {
        if buf.len() > 1 << 20 {
            return Err("request too large".into());
        }
        let n = stream.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("closed before headers".into());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(at) = find_subsequence(&buf, b"\r\n\r\n") {
            header_end = at;
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next().ok_or("empty request")?.to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().ok_or("no method")?.to_string();
    let target = parts.next().ok_or("no path")?.to_string();
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };
    let mut headers = Vec::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    let length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < length {
        let n = stream.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(length);
    Ok(HttpRequest { method, path, query, body, headers })
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

async fn respond(stream: &mut tokio::net::TcpStream, status: u16, body: &serde_json::Value) {
    use tokio::io::AsyncWriteExt;
    let text = serde_json::to_string(body).unwrap_or_else(|_| "{}".into());
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        _ => "Internal Server Error",
    };
    let out = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
        text.len()
    );
    let _ = stream.write_all(out.as_bytes()).await;
    let _ = stream.flush().await;
}

fn since_of(query: &str) -> usize {
    query
        .split('&')
        .find_map(|p| p.strip_prefix("since="))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// The whole API: small on purpose. The bot polls `/events`, starts turns,
/// answers approvals, and that is all a Telegram client needs.
async fn handle(
    stream: &mut tokio::net::TcpStream,
    req: HttpRequest,
    app: &Mutex<ServeApp>,
    state: &SharedState,
    gate: &Arc<ServeGate>,
    token: &Option<String>,
) {
    // Every arm computes its answer while the guards are held, drops them, and
    // only then writes: a MutexGuard across an await would make the whole
    // connection future non-Send.
    let (status, body) = {
        if !authorized(token, &req.headers) && req.path != "/health" {
            (401u16, serde_json::json!({"error": "unauthorized"}))
        } else {
            match (req.method.as_str(), req.path.as_str()) {
                ("GET", "/health") => (
                    200u16,
                    serde_json::json!({"ok": true, "version": flashagent_svc::updater::current_version()}),
                ),
                ("GET", "/status") => {
                    let (running, events_len, text_chars, turns_done, last_error, session, model) = {
                        let st = state.lock().expect("serve state");
                        let app = app.lock().expect("serve app");
                        (
                            st.running,
                            st.events.len(),
                            st.text.chars().count(),
                            st.turns_done,
                            st.last_error.clone(),
                            app.session_id.clone(),
                            app.model.clone(),
                        )
                    };
                    (
                        200u16,
                        serde_json::json!({
                            "running": running,
                            "session": session,
                            "model": model,
                            "events": events_len,
                            "text_chars": text_chars,
                            "turns_done": turns_done,
                            "last_error": last_error,
                        }),
                    )
                }
                ("GET", "/events") => {
                    let since = since_of(&req.query);
                    let st = state.lock().expect("serve state");
                    (
                        200u16,
                        serde_json::json!({
                            "running": st.running,
                            "events": st.events.iter().skip(since).cloned().collect::<Vec<_>>(),
                            "text": st.text,
                            "pending": st.pending,
                            "last_error": st.last_error,
                        }),
                    )
                }
                ("POST", "/turn") => {
                    let text = serde_json::from_slice::<serde_json::Value>(&req.body)
                        .ok()
                        .and_then(|v| v.get("text").and_then(|t| t.as_str()).map(str::to_string));
                    let text = text.unwrap_or_default();
                    if text.trim().is_empty() {
                        return respond(
                            stream,
                            400,
                            &serde_json::json!({"error": "body needs {\"text\": ...}"}),
                        )
                        .await;
                    }
                    let mut app = app.lock().expect("serve app");
                    match app.start_turn(&text) {
                        Ok(()) => (200u16, serde_json::json!({"ok": true})),
                        Err(why) => (409u16, serde_json::json!({"error": why})),
                    }
                }
                ("POST", "/approve") => {
                    let allow = serde_json::from_slice::<serde_json::Value>(&req.body)
                        .ok()
                        .and_then(|v| v.get("allow").and_then(|a| a.as_bool()))
                        .unwrap_or(false);
                    (200u16, serde_json::json!({"answered": gate.answer(allow)}))
                }
                ("POST", "/cancel") => {
                    let app = app.lock().expect("serve app");
                    app.cancel.store(true, Ordering::Relaxed);
                    (200u16, serde_json::json!({"ok": true}))
                }
                ("POST", "/reset") => {
                    let mut app = app.lock().expect("serve app");
                    if app.reset() {
                        (200u16, serde_json::json!({"ok": true, "session": app.session_id}))
                    } else {
                        (409u16, serde_json::json!({"error": "a turn is running"}))
                    }
                }
                _ => (404u16, serde_json::json!({"error": "no such route"})),
            }
        }
    };
    respond(stream, status, &body).await;
}

pub(crate) struct ServeOpts {
    pub port: u16,
    pub dir: Option<PathBuf>,
    /// Overrides `config.prompt_detail` for this instance: a room that runs
    /// often runs on the compact tier.
    pub prompt: Option<String>,
    /// Overrides `config.permission_mode`: a room's brain may be more or
    /// less trusted than the TUI's.
    pub mode: Option<String>,
    /// Overrides `config.thinking_effort`: a bot answering a group chat
    /// reasons at the room's pace, not the owner's.
    pub effort: Option<String>,
}

/// The permission mode a `--mode` flag names, or None when it names nothing.
fn parse_mode(raw: &str) -> Option<flashagent_core::PermissionMode> {
    use flashagent_core::PermissionMode;
    match raw.to_lowercase().as_str() {
        "planning" => Some(PermissionMode::Planning),
        "manual" => Some(PermissionMode::Manual),
        "accept" | "acceptedits" | "accept_edits" => Some(PermissionMode::AcceptEdits),
        "bypass" | "acceptall" | "accept_all" => Some(PermissionMode::Bypass),
        _ => None,
    }
}

/// `--serve`: no screens, no terminal, one session and one HTTP API. The
/// config must already be set up (`flashagent --setup`); a missing model is
/// an error, because a server with no brain is a blank screen with a port.
pub(crate) async fn run_serve(mut config: flashagent_core::AppConfig, opts: ServeOpts) -> Result<()> {
    if let Some(detail) = &opts.prompt {
        config.prompt_detail = detail.clone();
    }
    if let Some(mode) = &opts.mode {
        config.permission_mode = parse_mode(mode)
            .unwrap_or(flashagent_core::PermissionMode::AcceptEdits);
    }
    flashagent_tui::i18n::set(flashagent_tui::i18n::Lang::from_code(&config.interface_language));
    trace("serve: start");
    if config.active_profile().model.trim().is_empty() {
        anyhow::bail!("no model configured: run `flashagent --setup` first");
    }
    let cwd = match &opts.dir {
        Some(dir) => dir.canonicalize().unwrap_or_else(|_| dir.clone()),
        None => std::env::current_dir()?,
    };
    let cwd_display = cwd.display().to_string();
    let endpoint = config.endpoint();
    let remote =
        flashagent_core::url_host(&endpoint.url).is_some_and(|host| !flashagent_core::is_local_host(&host));
    let model = config.active_profile().model.clone();
    let source = Arc::new(crate::backend::BackendSource(flashagent_llm::Client::new(endpoint.clone(), "")));
    source.0.set_max_retries(config.network_retries);
    source.0.set_session_id(crate::sessions::new_session_id());
    let session_id = crate::sessions::new_session_id();

    // What the model can do decides the prompt's detail and the thinking
    // section. A server that does not answer in ten seconds runs without that
    // knowledge, as the TUI does.
    trace("serve: discovering the server");
    let discovery =
        tokio::time::timeout(Duration::from_secs(10), source.0.discover_server()).await.ok().flatten();
    trace("serve: discovery done");
    let capacity = discovery
        .as_ref()
        .and_then(|d| d.active_model.as_ref().and_then(|m| m.prompt_budget()))
        .unwrap_or(0);
    let reasoning_supported = discovery
        .as_ref()
        .and_then(|d| d.active_model.as_ref().map(|m| m.thinking.supported))
        .unwrap_or(false);
    if let Some(disc) = &discovery {
        source.0.adopt_discovery(disc);
    }
    source.0.set_model(&model);
    let effort = opts.effort.clone().unwrap_or_else(|| config.thinking_effort.clone());

    // The working folder is read three times, each read behind a clock: a slow
    // folder costs its rules and its outline, not the server.
    let mcp = {
        let cwd = cwd.clone();
        tokio::time::timeout(Duration::from_secs(2), tokio::task::spawn_blocking(move || {
            flashagent_tools::mcp::McpManager::new(cwd)
        }))
        .await
        .ok()
        .and_then(|r| r.ok())
    };
    trace("serve: mcp manager ready");
    let tools = Arc::new(flashagent_tools::BuiltinTools::new(flashagent_tools::BuiltinToolsConfig {
        cwd: cwd.clone(),
        brave_api_key: std::env::var("BRAVE_API_KEY").ok().filter(|k| !k.trim().is_empty()),
        question_gate: None,
        is_goal_mode: None,
        web_enabled: Some(config.web_tools),
        context_window: Some(capacity),
        mcp_manager: mcp.clone(),
        disabled_tools: Some(config.disabled_tools.clone()),
        home: None,
    })?);
    trace("serve: tools ready");

    let state: SharedState = Arc::new(Mutex::new(ServeState::default()));
    let gate = Arc::new(ServeGate {
        state: state.clone(),
        responders: Mutex::new(Vec::new()),
        counter: AtomicU64::new(0),
        timeout: Duration::from_secs(300),
    });
    let perm_state =
        Arc::new(flashagent_core::permissions::PermissionState::new(config.permission_mode, gate.clone()));
    perm_state.set_project_root(cwd.clone());
    let perm: &'static flashagent_core::permissions::PermissionedTools = Box::leak(Box::new(
        flashagent_core::permissions::PermissionedTools::new(tools.clone(), Some(tools.clone()), perm_state),
    ));

    // The system prompt: memory and outline are read behind clocks, the same
    // way the TUI reads them.
    trace("serve: reading memory");
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default();
    let memory_block = {
        let (m, c, h) = (model.clone(), cwd.clone(), home.clone());
        let budget = config.token_budget;
        match tokio::time::timeout(
            Duration::from_secs(2),
            tokio::task::spawn_blocking(move || flashagent_core::memory_context(&m, &c, &h, budget)),
        )
        .await
        {
            Ok(Ok((block, _))) => block,
            _ => flashagent_core::memory_context(&model, &std::env::temp_dir(), &home, config.token_budget).0,
        }
    };
    let outline = {
        let cwd = cwd.clone();
        match tokio::time::timeout(
            Duration::from_secs(2),
            tokio::task::spawn_blocking(move || flashagent_core::plan::project_outline(&cwd, 1_500)),
        )
        .await
        {
            Ok(outline) => outline.unwrap_or_default(),
            _ => String::new(),
        }
    };
    let detail = flashagent_core::PromptDetail::parse(&config.prompt_detail)
        .unwrap_or(flashagent_core::PromptDetail::auto(!remote, capacity, &config.prompt_detail));
    let system_prompt = flashagent_core::build_system_prompt(
        &flashagent_core::SystemPromptConfig {
            cwd: Some(cwd_display.clone()),
            platform: Some(flashagent_tools::shell::platform()),
            model: Some(model.clone()),
            effort: Some(effort.clone()),
            personality: None,
            memory: Some(memory_block),
            plan: Some(outline),
            uses_emoji: false,
            subagents_available: false,
            ask_user_available: false,
            goal_mode: false,
            detail,
            reasoning_supported,
            local: !remote,
        }
        .with_personality(&config.personality),
    );
    trace("serve: prompt ready");

    let mut app = ServeApp {
        state: state.clone(),
        source,
        perm,
        cancel: Arc::new(AtomicBool::new(false)),
        session_id,
        cwd_display,
        config: config.clone(),
        model: model.clone(),
        effort,
        system_prompt,
    };
    let _ = &mut app;

    let addr = format!("127.0.0.1:{}", opts.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let token = std::env::var("FLASHAGENT_SERVE_TOKEN").ok().filter(|t| !t.trim().is_empty());
    println!("FlashAgent serve on http://{addr} (session {})", app.session_id);
    trace("serve: listening");
    let app = Arc::new(Mutex::new(app));
    let server_app = app.clone();
    let server_state = state.clone();
    let server_gate = gate.clone();
    loop {
        let (mut stream, _) = listener.accept().await?;
        let app = server_app.clone();
        let state = server_state.clone();
        let gate = server_gate.clone();
        let token = token.clone();
        tokio::spawn(async move {
            match read_request(&mut stream).await {
                Ok(req) => handle(&mut stream, req, &app, &state, &gate, &token).await,
                Err(_) => respond(&mut stream, 400, &serde_json::json!({"error": "bad request"})).await,
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flashagent_core::permissions::{ApprovalGate, ApprovalRequest};

    #[tokio::test]
    async fn an_unanswered_approval_is_denied_when_the_clock_runs_out() {
        let gate = ServeGate {
            state: Arc::new(Mutex::new(ServeState::default())),
            responders: Mutex::new(Vec::new()),
            counter: AtomicU64::new(0),
            timeout: Duration::from_millis(80),
        };
        let req = ApprovalRequest {
            tool: "run_shell".into(),
            args_json: r#"{"command":"rm -rf /"}"#.into(),
            category: flashagent_core::permissions::Category::Shell,
            diff: None,
        };
        assert!(matches!(gate.approve(&req).await, flashagent_core::permissions::Decision::Deny));
        // The feed says the wait ended, so the bot can tell the person why.
        let st = gate.state.lock().unwrap();
        assert_eq!(st.pending, None);
        let shown = format!("{:?}", st.events);
        assert!(shown.contains("approval_timeout"), "{shown}");
    }

    #[tokio::test]
    async fn an_approval_answered_through_the_api_allows_the_call() {
        let gate = Arc::new(ServeGate {
            state: Arc::new(Mutex::new(ServeState::default())),
            responders: Mutex::new(Vec::new()),
            counter: AtomicU64::new(0),
            timeout: Duration::from_secs(10),
        });
        let req = ApprovalRequest {
            tool: "edit".into(),
            args_json: "{}".into(),
            category: flashagent_core::permissions::Category::Write,
            diff: Some("+ hello".into()),
        };
        let waiter = {
            let gate = gate.clone();
            let req = req.clone();
            tokio::spawn(async move { gate.approve(&req).await })
        };
        // The pending approval shows in the feed with its diff.
        tokio::time::sleep(Duration::from_millis(60)).await;
        let (pending_tool, pending_diff) = {
            let st = gate.state.lock().unwrap();
            let pending = st.pending.clone().expect("the approval is in the feed");
            (pending["tool"].clone(), pending["diff"].clone())
        };
        assert_eq!(pending_tool, "edit");
        assert_eq!(pending_diff, "+ hello");
        assert!(gate.answer(true), "the answer lands on the waiting call");
        assert!(matches!(waiter.await.unwrap(), flashagent_core::permissions::Decision::Allow));
    }

    #[test]
    fn the_token_is_checked_on_every_route_but_health() {
        let token = Some("secret".to_string());
        let with = vec![("Authorization".into(), "Bearer secret".into())];
        let without: Vec<(String, String)> = Vec::new();
        assert!(authorized(&token, &with));
        assert!(!authorized(&token, &without));
        assert!(authorized(&None, &without), "no token configured: the bind is local");
    }

    #[test]
    fn a_loop_event_becomes_a_feed_entry_with_its_shape() {
        let started = event_to_json(&flashagent_core::LoopEvent::ToolStarted {
            id: "t".into(),
            name: "read".into(),
            args_json: r#"{"path":"src/main.rs"}"#.into(),
        });
        assert_eq!(started["type"], "tool_start");
        assert_eq!(started["name"], "read");
        assert_eq!(started["args"]["path"], "src/main.rs");
        let done = event_to_json(&flashagent_core::LoopEvent::Done(flashagent_core::DoneReason::Completed));
        assert_eq!(done["type"], "done");
        assert_eq!(done["reason"], "Completed");
        let delta = event_to_json(&flashagent_core::LoopEvent::TurnDelta("hi".into()));
        assert_eq!(delta["type"], "text");
        assert_eq!(delta["text"], "hi");
    }

    #[test]
    fn the_since_parameter_counts_events_not_bytes() {
        assert_eq!(since_of(""), 0);
        assert_eq!(since_of("since=12"), 12);
        assert_eq!(since_of("a=1&since=7"), 7);
    }
}
