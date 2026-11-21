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
    /// Which session's feed the request belongs to; the permission layer is
    /// built once for the whole serve, so the gate learns the session per
    /// call instead of capturing a state at start-up.
    session: usize,
    state: SharedState,
    respond: tokio::sync::oneshot::Sender<flashagent_core::permissions::Decision>,
}

/// How many feed entries stay readable. The bot polls every second, so a
/// few thousand cover a long stall; the rest is dead weight in RSS forever.
const FEED_RETAIN: usize = 4096;

/// What the HTTP API reads. One guard over everything the bot polls.
#[derive(Default)]
struct ServeState {
    running: bool,
    events: Vec<serde_json::Value>,
    /// How many events were dropped off the front of `events`. The feed is
    /// addressed by an absolute cursor (`events_base + index`), so trimming
    /// the head never renumbers what the client already consumed.
    events_base: usize,
    /// The turn's answer so far: every `TurnDelta` in order.
    text: String,
    pending: Option<serde_json::Value>,
    /// The history the last finished turn left behind; the next turn starts
    /// from it, which is what makes one process a continuing session.
    history: Option<Vec<ChatMessage>>,
    last_error: Option<String>,
    turns_done: u64,
    /// The live turn's steering channel. Gone between turns: a steer into an
    /// idle brain is a 409 the bot shows the room, not a queue nobody drains.
    steer_tx: Option<tokio::sync::mpsc::UnboundedSender<flashagent_core::Steer>>,
}

impl ServeState {
    /// Append one feed entry, trimming the head once the feed doubles the
    /// retention floor — amortized, not a drain on every push.
    fn push_event(&mut self, event: serde_json::Value) {
        self.events.push(event);
        if self.events.len() > FEED_RETAIN * 2 {
            let drop = self.events.len() - FEED_RETAIN;
            self.events.drain(..drop);
            self.events_base += drop;
        }
    }

    fn total_events(&self) -> usize {
        self.events_base + self.events.len()
    }

    /// The entries the client has not consumed yet, by absolute cursor. A
    /// cursor older than the retained window gets the window from its start:
    /// replies are deduplicated by the bot, and the turn's text is read from
    /// the feed's `text` field, so a gap is harmless.
    fn events_since(&self, since: usize) -> Vec<serde_json::Value> {
        let start = since.saturating_sub(self.events_base).min(self.events.len());
        self.events[start..].to_vec()
    }
}

type SharedState = Arc<Mutex<ServeState>>;

tokio::task_local! {
    /// The session whose turn owns the current task. The permission layer is
    /// process-wide (the tools are built once), so the approval gate finds
    /// the feed of the turn that called the tool through this instead of a
    /// state captured at start-up — every session has its own feed.
    static CURRENT_SESSION: SharedState;
}

fn session_key(state: &SharedState) -> usize {
    Arc::as_ptr(state) as usize
}

fn current_session() -> Option<SharedState> {
    CURRENT_SESSION.try_with(Clone::clone).ok()
}

/// The approval gate for a session nobody sits at: the request goes into the
/// session's event feed, the client answers through `POST /approve`, and a
/// call that waits past the clock is denied — the model adapts to a refusal,
/// a hung start-up it cannot adapt to.
struct ServeGate {
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
    ) -> SharedState {
        let state = current_session().unwrap_or_default();
        self.responders.lock().expect("gate responders").push(PendingApproval {
            session: session_key(&state),
            state: state.clone(),
            respond,
        });
        let event = serde_json::json!({
            "type": "approval",
            "id": id,
            "tool": tool,
            "args_json": serde_json::from_str::<serde_json::Value>(args_json)
                .ok()
                .unwrap_or_default(),
            "diff": diff,
        });
        {
            let mut st = state.lock().expect("serve state");
            st.pending = Some(event.clone());
            st.push_event(event);
        }
        state
    }

    /// `POST /approve` names a session: its waiting call is the one to answer.
    fn answer_for(&self, state: &SharedState, allow: bool) -> bool {
        use flashagent_core::permissions::Decision;
        let key = session_key(state);
        let pending = {
            let mut responders = self.responders.lock().expect("gate responders");
            match responders.iter().rposition(|p| p.session == key) {
                Some(pos) => responders.remove(pos),
                None => return false,
            }
        };
        let _ = pending.respond.send(if allow { Decision::Allow } else { Decision::Deny });
        let mut st = pending.state.lock().expect("serve state");
        st.pending = None;
        st.push_event(serde_json::json!({"type": "approval_answered", "allow": allow}));
        true
    }
}

#[async_trait::async_trait]
impl flashagent_core::permissions::ApprovalGate for ServeGate {
    async fn approve(&self, req: &flashagent_core::permissions::ApprovalRequest) -> flashagent_core::permissions::Decision {
        use flashagent_core::permissions::Decision;
        let id = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let state = self.push(id, &req.tool, &req.args_json, req.diff.clone(), tx);
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(decision)) => decision,
            _ => {
                // The answer never came; the feed says the wait is over.
                let mut st = state.lock().expect("serve state");
                st.pending = None;
                st.push_event(serde_json::json!({"type": "approval_timeout", "id": id, "tool": req.tool}));
                Decision::Deny
            }
        }
    }
}

/// A question waiting for the chat (`user_interaction` mode ask). The tool
/// blocks, the client answers through `POST /answer`, and the answer is the
/// client's formatted vote result — the model reads the tally, not a lone
/// option, when several people voted.
struct PendingQuestion {
    id: u64,
    session: usize,
    respond: tokio::sync::oneshot::Sender<String>,
}

#[derive(Default)]
struct ServeQuestions {
    pending: Mutex<Vec<PendingQuestion>>,
    counter: AtomicU64,
    /// The hard backstop: the bot's own card policy closes earlier; this only
    /// keeps a forgetful or restarted client from hanging the turn forever.
    timeout: std::sync::OnceLock<Duration>,
}

impl ServeQuestions {
    fn with_timeout(timeout: Duration) -> Self {
        let q = Self::default();
        let _ = q.timeout.set(timeout);
        q
    }

    fn answer_for(&self, state: &SharedState, id: u64, answer: String) -> bool {
        let key = session_key(state);
        let pending = {
            let mut pending = self.pending.lock().expect("questions");
            // An ask whose tool already timed out is dead: drop it.
            pending.retain(|p| !p.respond.is_closed());
            match pending.iter().position(|p| p.id == id && p.session == key) {
                Some(pos) => pending.remove(pos),
                None => return false,
            }
        };
        let _ = pending.respond.send(answer);
        true
    }
}

#[async_trait::async_trait]
impl flashagent_tools::ask_user::QuestionGate for ServeQuestions {
    async fn ask(
        &self,
        question: &str,
        options: Option<&[String]>,
        multi_select: bool,
        _deadline: Option<std::time::Instant>,
    ) -> Result<(String, bool), String> {
        let state = current_session()
            .ok_or_else(|| "no session is running this question".to_string())?;
        let id = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut st = state.lock().expect("serve state");
            st.push_event(serde_json::json!({
                "type": "ask",
                "id": id,
                "question": question,
                "options": options,
                "multi_select": multi_select,
            }));
        }
        self.pending.lock().expect("questions").push(PendingQuestion {
            id,
            session: session_key(&state),
            respond: tx,
        });
        let wait = self
            .timeout
            .get()
            .copied()
            .unwrap_or(Duration::from_secs(360));
        match tokio::time::timeout(wait, rx).await {
            Ok(Ok(answer)) => Ok((answer, false)),
            _ => {
                self.pending.lock().expect("questions").retain(|p| p.id != id);
                Err("nobody answered in the chat before the deadline".into())
            }
        }
    }

    async fn notify(&self, level: &str, text: &str) -> Result<(), String> {
        let state = current_session()
            .ok_or_else(|| "no session is running this notice".to_string())?;
        state.lock().expect("serve state").push_event(serde_json::json!({
            "type": "notice",
            "level": level,
            "text": text,
        }));
        Ok(())
    }
}

/// The map from a loop event to what the client polls.
use flashagent_core::loop_::ToolExec as _;

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
        LoopEvent::ToolFinished { is_error, result_len, result, .. } => {
            // The verdict line rides along, capped: the bot watches a
            // `github` result for the issue it just adopted, and a log line
            // beats a bare length when a room asks what happened.
            serde_json::json!({
                "type": "tool_end",
                "ok": !is_error,
                "result_len": result_len,
                "result": result
                    .as_deref()
                    .map(|r| r.chars().take(300).collect::<String>()),
            })
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

/// What every session shares: the backend, the permission layer, the prompt.
/// A session adds its own feed, history, approvals and cancellation on top.
struct ServeBase {
    source: Arc<crate::backend::BackendSource>,
    source_dyn: Arc<dyn flashagent_core::LlmSource>,
    perm: &'static flashagent_core::permissions::PermissionedTools,
    /// One gate for the whole serve: it routes each approval to the session
    /// whose turn called the tool, through the task-local.
    gate: Arc<ServeGate>,
    /// The inline question card, routed the same way.
    questions: Arc<ServeQuestions>,
    /// The last saved session id per key, so a restarted serve (a reboot, an
    /// update) resumes the conversation it was having instead of a blank one.
    session_map: Arc<Mutex<std::collections::HashMap<String, String>>>,
    session_map_path: PathBuf,
    config: flashagent_core::AppConfig,
    model: String,
    effort: String,
    cwd_display: String,
    system_prompt: String,
    /// `(capacity, fixed, threshold_pct)` when the operator bounded the
    /// window with `--compact-at`; `None` keeps every session uncompacted.
    compactor: Option<(usize, usize, usize)>,
    /// The room's profile, for `/status` and nothing else: the constants it
    /// brought were already poured into the base at start.
    profile: Option<&'static TgbotProfile>,
}

/// One named brain of the serve: `main`, `report-1234`, ... A room's busy
/// hour cannot bleed into a /report discussion — the discussion runs in its
/// own feed and history.
struct Session {
    session_id: String,
    state: SharedState,
    gate: Arc<ServeGate>,
    app: Arc<Mutex<ServeApp>>,
}

impl Session {
    fn new(base: &ServeBase, key: &str) -> Self {
        let state: SharedState = Arc::new(Mutex::new(ServeState::default()));
        // A restarted serve resumes the last saved conversation of this key.
        // A throwaway service session has nothing worth resuming.
        let saved_id = if key.starts_with("svc-") {
            None
        } else {
            base.session_map
                .lock()
                .expect("session map")
                .get(key)
                .cloned()
        };
        let session_id = saved_id
            .clone()
            .unwrap_or_else(|| format!("{key}-{}", crate::sessions::new_session_id()));
        if let Some(id) = &saved_id {
            if let Some(dir) = crate::sessions::sessions_dir() {
                if let Some(history) = load_serve_history(&dir, id, &base.system_prompt) {
                    state.lock().expect("serve state").history = Some(history);
                }
            }
        }
        let app = Arc::new(Mutex::new(ServeApp::new(
            base,
            state.clone(),
            session_id.clone(),
            key.to_string(),
        )));
        // The approval buttons answer the same process-wide gate the
        // permission layer calls; the state names the session.
        Self { session_id, state, gate: base.gate.clone(), app }
    }
}

/// The saved conversation of a session, with the system prompt rebuilt: the
/// prompt, AGENTS and memory may have changed since the save, the turns have
/// not. None when there is no readable session or it holds only its prompt.
fn load_serve_history(
    dir: &std::path::Path,
    id: &str,
    system_prompt: &str,
) -> Option<Vec<ChatMessage>> {
    let saved = crate::sessions::read_session(dir, id).ok()?;
    let mut messages: Vec<ChatMessage> = saved
        .messages
        .into_iter()
        .map(ChatMessage::from)
        .collect();
    if messages
        .first()
        .is_some_and(|m| m.role == flashagent_llm::Role::System)
    {
        messages.remove(0);
    }
    if messages.is_empty() {
        return None;
    }
    messages.insert(0, ChatMessage::system(system_prompt.to_string()));
    Some(messages)
}

fn save_session_map(
    map: &Mutex<std::collections::HashMap<String, String>>,
    path: &std::path::Path,
) {
    let guard = map.lock().expect("session map");
    if let Ok(text) = serde_json::to_string_pretty(&*guard) {
        let _ = std::fs::write(path, text);
    }
}

fn remember_session(
    map: &Mutex<std::collections::HashMap<String, String>>,
    path: &std::path::Path,
    key: &str,
    id: &str,
) {
    if key.starts_with("svc-") {
        return;
    }
    {
        let mut guard = map.lock().expect("session map");
        if guard.get(key).is_some_and(|v| v == id) {
            return;
        }
        guard.insert(key.to_string(), id.to_string());
    }
    save_session_map(map, path);
}

fn forget_session(
    map: &Mutex<std::collections::HashMap<String, String>>,
    path: &std::path::Path,
    key: &str,
) {
    if key.starts_with("svc-") {
        return;
    }
    let changed = {
        let mut guard = map.lock().expect("session map");
        guard.remove(key).is_some()
    };
    if changed {
        save_session_map(map, path);
    }
}

/// The named session a request addresses: the `session` query parameter for
/// GETs, the `session` body field for POSTs, `main` when nothing is named.
fn session_key_of(query: &str, body: &serde_json::Value) -> String {
    let candidates = [
        query
            .split('&')
            .find_map(|p| p.strip_prefix("session="))
            .map(str::to_string),
        body.get("session")
            .and_then(|s| s.as_str())
            .map(str::to_string),
    ];
    for key in candidates.into_iter().flatten() {
        if !key.trim().is_empty() {
            return key;
        }
    }
    "main".to_string()
}

/// Get-or-create the named session.
fn session_for(
    sessions: &Mutex<std::collections::HashMap<String, Arc<Session>>>,
    base: &ServeBase,
    key: &str,
) -> Arc<Session> {
    let mut map = sessions.lock().expect("serve sessions");
    map.entry(key.to_string())
        .or_insert_with(|| Arc::new(Session::new(base, key)))
        .clone()
}

/// The headless session: history, tools and the loop, behind the HTTP API.
struct ServeApp {
    state: SharedState,
    source: Arc<crate::backend::BackendSource>,
    perm: &'static flashagent_core::permissions::PermissionedTools,
    cancel: Arc<AtomicBool>,
    session_id: String,
    session_key: String,
    session_map: Arc<Mutex<std::collections::HashMap<String, String>>>,
    session_map_path: PathBuf,
    cwd_display: String,
    config: flashagent_core::AppConfig,
    model: String,
    effort: String,
    /// The system prompt, built once at start-up: memory, outline, identity.
    /// The turn's history starts with it, like the TUI's does.
    system_prompt: String,
    /// Mid-turn compaction for rooms with a bounded window (`--context-cap`):
    /// the TUI's TurnCompactor, pointed at the same session archive.
    compactor: Option<Arc<crate::compact::TurnCompactor>>,
}

impl ServeApp {
    /// One fresh brain for the named session: empty feed, empty history,
    /// its own approvals and cancellation.
    fn new(
        base: &ServeBase,
        state: SharedState,
        session_id: String,
        session_key: String,
    ) -> Self {
        let compactor = base.compactor.map(|(capacity, fixed, threshold_pct)| {
            Arc::new(crate::compact::TurnCompactor {
                source: base.source_dyn.clone(),
                archive: crate::sessions::session_path(&session_id),
                capacity,
                fixed,
                threshold_pct,
                report: Arc::new(Mutex::new(None)),
                sink: None,
                model: base.model.clone(),
            })
        });
        Self {
            state,
            source: base.source.clone(),
            perm: base.perm,
            cancel: Arc::new(AtomicBool::new(false)),
            session_id,
            session_key,
            session_map: base.session_map.clone(),
            session_map_path: base.session_map_path.clone(),
            cwd_display: base.cwd_display.clone(),
            config: base.config.clone(),
            model: base.model.clone(),
            effort: base.effort.clone(),
            system_prompt: base.system_prompt.clone(),
            compactor,
        }
    }

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
            st.push_event(serde_json::json!({"type": "turn_started"}));
        }
        let turn_opts = build_turn_options(&self.config, &self.effort);
        let (steer_tx, steer_rx) = tokio::sync::mpsc::unbounded_channel();
        {
            // The channel lives exactly as long as the turn: /steer finds it
            // while `running`, the turn end clears it.
            let mut st = self.state.lock().expect("serve state");
            st.steer_tx = Some(steer_tx);
        }
        let state = self.state.clone();
        let source = self.source.clone();
        let cancel = self.cancel.clone();
        let shared = flashagent_core::loop_::SharedHistory::new(history.clone());
        let session_id = self.session_id.clone();
        let cwd_display = self.cwd_display.clone();
        let model = self.model.clone();
        let perm = self.perm;
        let compactor = self.compactor.clone();
        let session_map = self.session_map.clone();
        let session_map_path = self.session_map_path.clone();
        let session_key = self.session_key.clone();
        tokio::spawn(async move {
            let loop_config =
                flashagent_core::loop_::LoopConfig { base_turn_options: turn_opts, ..Default::default() };
            let mut loop_ =
                flashagent_core::loop_::AgentLoop::with_steering(loop_config, cancel, steer_rx);
            if let Some(compactor) = compactor {
                loop_ = loop_.with_compactor(compactor);
            }
            let state_for_events = state.clone();
            let mut on_event = |event: flashagent_core::LoopEvent| {
                let mut st = state_for_events.lock().expect("serve state");
                // A chat_reply call IS the delivery: the event stream of the
                // session it ran in carries it as a `reply` with the full
                // text — the bot posts it to that exact message.
                if let flashagent_core::LoopEvent::ToolStarted { name, args_json, .. } = &event {
                    if name == "chat_reply" {
                        if let Some(reply) = chat_reply_event(args_json) {
                            st.push_event(reply);
                        }
                    }
                }
                if let flashagent_core::LoopEvent::TurnDelta(text) = &event {
                    st.text.push_str(text);
                }
                st.push_event(event_to_json(&event));
            };
            // The approval gate finds this session through the task-local:
            // the tools are shared by every session, the feed is not. The
            // turn's mutation budget (telegram/github caps) rides the same
            // way: each turn starts with a fresh one, parallel turns of
            // parallel sessions never share it.
            let run_state = state.clone();
            let res = flashagent_tools::scope_turn(async move {
                CURRENT_SESSION
                    .scope(run_state, async move {
                        loop_.run(source.as_ref(), perm, shared, &mut on_event).await
                    })
                    .await
            })
            .await;
            let mut st = state.lock().expect("serve state");
            st.running = false;
            st.steer_tx = None;
            st.turns_done += 1;
            match res {
                Ok((shared, reason)) => {
                    let snapshot = shared.snapshot();
                    st.history = Some(snapshot.clone());
                    st.push_event(serde_json::json!({"type": "turn_finished", "reason": format!("{reason:?}")}));
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
                    st.push_event(serde_json::json!({"type": "turn_failed"}));
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
            // The conversation is on disk and its id is remembered before the
            // next request can arrive: a restart from here resumes this turn.
            remember_session(&session_map, &session_map_path, &session_key, &session_id);
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
        forget_session(&self.session_map, &self.session_map_path, &self.session_key);
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

/// POST /steer: words into a turn that is already running — the "умный
/// steering" the bot speaks. A steer into an idle brain is a 409, not a
/// queue: the bot only steers what it saw while the brain was answering.
fn steer_answer(state: &SharedState, body: &[u8]) -> (u16, serde_json::Value) {
    let text = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("text").and_then(|t| t.as_str()).map(str::to_string))
        .unwrap_or_default();
    if text.trim().is_empty() {
        return (
            400u16,
            serde_json::json!({"error": "body needs {\"text\": ...}"}),
        );
    }
    let st = state.lock().expect("serve state");
    match st.steer_tx.as_ref() {
        Some(tx) => {
            let _ = tx.send(flashagent_core::Steer::text(text));
            (200u16, serde_json::json!({"ok": true}))
        }
        None => (409u16, serde_json::json!({"error": "no turn is running"})),
    }
}

/// The compaction trigger as a percent of the window: an explicit
/// `--compact-at` wins (the owner's "55k of 65k"), the config's value or
/// the window's default follows.
fn threshold_pct_of(compact_at: Option<usize>, configured: usize, capacity: usize) -> usize {
    match compact_at {
        Some(at) if capacity > 0 && at <= capacity => (at * 100 / capacity).max(1),
        _ => flashagent_core::resolved_compact_threshold(configured, capacity),
    }
}

/// A chat_reply call becomes a `reply` feed entry: the message id and the
/// full text ride in the call's own arguments. `None` when the call does
/// not name a message — nothing to deliver.
fn chat_reply_event(args_json: &str) -> Option<serde_json::Value> {
    let args = serde_json::from_str::<serde_json::Value>(args_json).ok()?;
    let message_id = args.get("message_id").and_then(|v| v.as_i64())?;
    let text = args.get("text").and_then(|v| v.as_str()).unwrap_or_default();
    Some(serde_json::json!({
        "type": "reply",
        "message_id": message_id,
        "text": text,
    }))
}

/// What a POST /turn means, decided before any guard is dropped.
enum TurnPath {
    /// No turn runs: start one.
    Start,
    /// A turn runs and the prompt flew into it as a steer. The loop drains
    /// steers at the top of its next step, so the words ride in the same
    /// request as whatever tool results just landed — no separate endpoint
    /// dance for the client.
    Steered,
    /// A turn runs but its steering channel is gone (the few instructions
    /// between `running = true` and the spawn): nothing to fly into.
    BusyWithoutChannel,
}

/// POST /turn while a turn is already running is a steer by itself.
fn turn_path(state: &SharedState, text: &str) -> TurnPath {
    let st = state.lock().expect("serve state");
    if st.running {
        return match st.steer_tx.as_ref() {
            Some(tx) => {
                let _ = tx.send(flashagent_core::Steer::text(text.to_string()));
                TurnPath::Steered
            }
            None => TurnPath::BusyWithoutChannel,
        };
    }
    TurnPath::Start
}

/// The whole API: small on purpose. The bot polls `/events`, starts turns,
/// answers approvals, and that is all a Telegram client needs. Every route
/// addresses a session: the `session` query parameter or body field, `main`
/// when unnamed — a /report discussion lives in its own session and a busy
/// room cannot bleed into it.
async fn handle(
    stream: &mut tokio::net::TcpStream,
    req: HttpRequest,
    sessions: &Mutex<std::collections::HashMap<String, Arc<Session>>>,
    base: &ServeBase,
    token: &Option<String>,
) {
    // Every arm computes its answer while the guards are held, drops them, and
    // only then writes: a MutexGuard across an await would make the whole
    // connection future non-Send.
    let (status, body) = {
        if !authorized(token, &req.headers) && req.path != "/health" {
            (401u16, serde_json::json!({"error": "unauthorized"}))
        } else {
            let body_value = serde_json::from_slice::<serde_json::Value>(&req.body)
                .ok()
                .unwrap_or_default();
            let key = session_key_of(&req.query, &body_value);
            let session = session_for(sessions, base, &key);
            match (req.method.as_str(), req.path.as_str()) {
                ("GET", "/health") => (
                    200u16,
                    serde_json::json!({"ok": true, "version": flashagent_svc::updater::current_version()}),
                ),
                ("GET", "/status") => {
                    let (running, events_len, text_chars, turns_done, last_error, session_id, model) = {
                        let st = session.state.lock().expect("serve state");
                        let app = session.app.lock().expect("serve app");
                        (
                            st.running,
                            st.total_events(),
                            st.text.chars().count(),
                            st.turns_done,
                            st.last_error.clone(),
                            app.session_id.clone(),
                            app.model.clone(),
                        )
                    };
                    let total = sessions.lock().expect("serve sessions").len();
                    let tool_names: Vec<String> =
                        base.perm.specs().into_iter().map(|s| s.name).collect();
                    (
                        200u16,
                        serde_json::json!({
                            "running": running,
                            "session": session_id,
                            "key": key,
                            "sessions": total,
                            "profile": base.profile.map(|p| p.name),
                            "tools": tool_names,
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
                    let st = session.state.lock().expect("serve state");
                    (
                        200u16,
                        serde_json::json!({
                            "running": st.running,
                            // The absolute cursor: a client that saw `total`
                            // resumes from it even after the head was trimmed,
                            // and a smaller total than its cursor means the
                            // feed (the process) restarted.
                            "total": st.total_events(),
                            "events": st.events_since(since),
                            "text": st.text,
                            "pending": st.pending,
                            "last_error": st.last_error,
                        }),
                    )
                }
                ("POST", "/turn") => {
                    let text = body_value
                        .get("text")
                        .and_then(|t| t.as_str())
                        .unwrap_or_default()
                        .to_string();
                    if text.trim().is_empty() {
                        return respond(
                            stream,
                            400,
                            &serde_json::json!({"error": "body needs {\"text\": ...}"}),
                        )
                        .await;
                    }
                    match turn_path(&session.state, &text) {
                        TurnPath::Steered => {
                            (200u16, serde_json::json!({"ok": true, "steered": true}))
                        }
                        TurnPath::BusyWithoutChannel => (
                            409u16,
                            serde_json::json!({"error": "a turn is already running"}),
                        ),
                        TurnPath::Start => {
                            let mut app = session.app.lock().expect("serve app");
                            match app.start_turn(&text) {
                                Ok(()) => (200u16, serde_json::json!({"ok": true})),
                                Err(why) => (409u16, serde_json::json!({"error": why})),
                            }
                        }
                    }
                }
                ("POST", "/approve") => {
                    let allow = body_value
                        .get("allow")
                        .and_then(|a| a.as_bool())
                        .unwrap_or(false);
                    (
                        200u16,
                        serde_json::json!({"answered": session.gate.answer_for(&session.state, allow)}),
                    )
                }
                ("POST", "/answer") => {
                    // The card in the chat is resolved: hand the formatted
                    // vote result (or the chosen option) to the waiting
                    // user_interaction call of this session.
                    let id = body_value
                        .get("id")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let answer = body_value
                        .get("answer")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let answered = id > 0
                        && !answer.trim().is_empty()
                        && base.questions.answer_for(&session.state, id, answer);
                    (200u16, serde_json::json!({"answered": answered}))
                }
                ("POST", "/cancel") => {
                    let app = session.app.lock().expect("serve app");
                    app.cancel.store(true, Ordering::Relaxed);
                    (200u16, serde_json::json!({"ok": true}))
                }
                ("POST", "/reset") => {
                    let mut app = session.app.lock().expect("serve app");
                    if app.reset() {
                        (200u16, serde_json::json!({"ok": true, "session": app.session_id}))
                    } else {
                        (409u16, serde_json::json!({"error": "a turn is running"}))
                    }
                }
                ("POST", "/steer") => steer_answer(&session.state, &req.body),
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
    /// Overrides the model's reported window: a room's budget can be smaller
    /// than what the server allows (`--context-cap 65000`).
    pub context_cap: Option<usize>,
    /// The compaction trigger as an absolute token count (`--compact-at
    /// 55000`); without it the config's or the window's percentage applies.
    pub compact_at: Option<usize>,
    /// Tools this serve must not offer (`--disable a,b`): a room's brain does
    /// not need the whole TUI toolbox, and every extra tool is a failure mode.
    pub disable: Option<String>,
    /// Never ask a human: whatever the mode would send to an approval card is
    /// denied outright. The bot runs unattended; safety is policy, not Q&A.
    pub no_ask: bool,
    /// Trust this model with pictures even when the server's model list does
    /// not say it supports them. The gateway's metadata is sometimes stale;
    /// the room needs stickers and GIFs seen.
    pub vision: bool,
    /// A room bundle (`--profile tgbot-dev`): tier, gate, toolset, caps,
    /// language — every constant a Telegram serve needs, in one place, so the
    /// unit carries one flag instead of a scatter (B4/B7/B8).
    pub profile: Option<String>,
}

/// One Telegram room's constants. The unit names the profile; the flags the
/// operator still passes win over the profile's defaults (B3).
pub(crate) struct TgbotProfile {
    pub name: &'static str,
    /// B2: the dev room reads the Full tier, the users' room Standard.
    pub prompt_detail: &'static str,
    /// H9: the dev room reasons at full length, the users' room at low.
    pub effort: &'static str,
    /// The dev room runs bypass (its shell stays whole — the owner's call);
    /// the users' room is planning with an allowlist gate (B1).
    pub permission_mode: &'static str,
    /// B1: the users' room's own tools pass the gate silently, everything
    /// else is refused without a card.
    pub allowlist: &'static [&'static str],
    /// Tools that are not even offered: every extra schema is a failure mode
    /// and a token the room pays for on every turn (H13).
    pub disabled: &'static [&'static str],
    /// B3/H15: the users' room sits in a 256k window with compaction at 200k;
    /// the dev room keeps its whole history.
    pub context_cap: Option<usize>,
    pub compact_at: Option<usize>,
    /// Rooms see stickers and screenshots (the units passed --vision for it).
    pub vision: bool,
    /// A13: only the dev session acts outside its own room.
    pub cross_room: bool,
    /// B5: language and tone, said once here and never glued per turn.
    pub language_directive: &'static str,
}

const TG_LANGUAGE: &str = "ЯЗЫК И ТОН КОМНАТЫ: отвечай только по-русски, независимо от языка собеседника. Живой, дружелюбный тон без официоза; эмодзи не используй. На провокации и не по теме — короткий живой юмор, одна-две фразы, без холодных отписок.";

static TG_BOT_DEV: TgbotProfile = TgbotProfile {
    name: "tgbot-dev",
    prompt_detail: "full",
    effort: "high",
    permission_mode: "bypass",
    allowlist: &[],
    disabled: &["edit", "plan", "skill", "agent", "spawn_agent", "send_message", "review_agent"],
    context_cap: None,
    compact_at: None,
    vision: true,
    cross_room: true,
    language_directive: TG_LANGUAGE,
};

static TG_BOT_USERS: TgbotProfile = TgbotProfile {
    name: "tgbot-users",
    prompt_detail: "standard",
    effort: "low",
    permission_mode: "planning",
    allowlist: &[
        "telegram",
        "tg_journal",
        "github",
        "memory",
        "chat_reply",
        "user_interaction",
        "read",
        "search",
        "git",
        "env_info",
        "web",
        "plan",
        "task",
        "view_image",
    ],
    disabled: &[
        "run_shell",
        "edit",
        "http_request",
        "skill",
        "agent",
        "spawn_agent",
        "send_message",
        "review_agent",
    ],
    context_cap: Some(256_000),
    compact_at: Some(200_000),
    vision: true,
    cross_room: false,
    language_directive: TG_LANGUAGE,
};

/// The profile a `--profile` flag names, or None when it names nothing.
fn parse_profile(raw: &str) -> Option<&'static TgbotProfile> {
    match raw.trim().to_lowercase().as_str() {
        "tgbot-dev" => Some(&TG_BOT_DEV),
        "tgbot-users" => Some(&TG_BOT_USERS),
        _ => None,
    }
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
    // The profile is resolved once, here: an unknown name is a typo in the
    // unit, and a room that starts without its constants starts wrong (G14).
    let profile = match opts.profile.as_deref() {
        None => None,
        Some(raw) => match parse_profile(raw) {
            Some(p) => Some(p),
            None => anyhow::bail!(
                "неизвестный профиль '{raw}': бывают tgbot-dev и tgbot-users"
            ),
        },
    };
    if let Some(detail) = &opts.prompt {
        config.prompt_detail = detail.clone();
    } else if let Some(p) = profile {
        config.prompt_detail = p.prompt_detail.to_string();
    }
    if let Some(mode) = &opts.mode {
        config.permission_mode = parse_mode(mode)
            .unwrap_or(flashagent_core::PermissionMode::AcceptEdits);
    } else if let Some(p) = profile {
        config.permission_mode = parse_mode(p.permission_mode)
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

    // What the model can do decides the prompt's detail and the thinking
    // section. A server that does not answer in ten seconds runs without that
    // knowledge, as the TUI does.
    trace("serve: discovering the server");
    let discovery =
        tokio::time::timeout(Duration::from_secs(10), source.0.discover_server()).await.ok().flatten();
    trace("serve: discovery done");
    let capacity = opts
        .context_cap
        .or_else(|| profile.and_then(|p| p.context_cap))
        .unwrap_or(
            discovery
                .as_ref()
                .and_then(|d| d.active_model.as_ref().and_then(|m| m.prompt_budget()))
                .unwrap_or(0),
        );
    let reasoning_supported = discovery
        .as_ref()
        .and_then(|d| d.active_model.as_ref().map(|m| m.thinking.supported))
        .unwrap_or(false);
    if let Some(disc) = &discovery {
        source.0.adopt_discovery(disc);
    }
    source.0.set_model(&model);
    let effort = opts
        .effort
        .clone()
        .or_else(|| profile.map(|p| p.effort.to_string()))
        .unwrap_or_else(|| config.thinking_effort.clone());

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
    let mut disabled = config.disabled_tools.clone();
    if let Some(p) = profile {
        for name in p.disabled {
            if !disabled.iter().any(|n| n == name) {
                disabled.push(name.to_string());
            }
        }
    }
    if let Some(list) = opts.disable.as_deref() {
        disabled.extend(
            list.split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string),
        );
    }
    // The bot closes its card by itself; this timeout is only the backstop
    // that keeps a restarted or silent client from hanging the turn forever.
    let questions = Arc::new(ServeQuestions::with_timeout(Duration::from_secs(360)));
    // A tgbot room reaches the bot through the bridge; no secret in the
    // environment means the hands of the room cannot be built (G14: refuse to
    // start rather than start mute). The URL stays a local default.
    let tg_bridge = match profile {
        None => None,
        Some(p) => {
            let token = std::env::var("FLASHAGENT_TG_BRIDGE_TOKEN")
                .ok()
                .filter(|t| !t.trim().is_empty());
            let Some(token) = token else {
                anyhow::bail!(
                    "профиль {} требует секрет моста: положи FLASHAGENT_TG_BRIDGE_TOKEN в окружение юнита",
                    p.name
                );
            };
            let base_url = std::env::var("FLASHAGENT_TG_BRIDGE_URL")
                .ok()
                .filter(|u| !u.trim().is_empty())
                .unwrap_or_else(|| "http://127.0.0.1:8777".to_string());
            Some(flashagent_tools::TgBridgeConfig {
                base_url,
                token,
                cross_room: p.cross_room,
            })
        }
    };
    // The GitHub issues connection: the room's tracker memory. Optional by
    // nature — a room without a token runs without the tool, one with a
    // token gets it.
    let github = flashagent_tools::GitHubConfig::from_env();
    let tools = Arc::new(flashagent_tools::BuiltinTools::new(flashagent_tools::BuiltinToolsConfig {
        cwd: cwd.clone(),
        brave_api_key: std::env::var("BRAVE_API_KEY").ok().filter(|k| !k.trim().is_empty()),
        // `user_interaction` reaches the chat card: the model asks the room
        // and gets the vote result back in the same turn.
        question_gate: Some(questions.clone() as Arc<dyn flashagent_tools::QuestionGate>),
        is_goal_mode: None,
        web_enabled: Some(config.web_tools),
        context_window: Some(capacity),
        mcp_manager: mcp.clone(),
        disabled_tools: Some(disabled),
        home: None,
        // Every session's event stream carries its own `chat_reply` calls as
        // `reply` events — the outbox is the feed itself, per session.
        chat_enabled: true,
        tg_bridge,
        github,
    })?);
    // The same rule the TUI applies: a model the server reports as seeing
    // images gets `read mode=image`; a model it never mentioned is assumed to
    // see, and the server's own error is better than a wrong refusal. Serve
    // forgot this, so every screenshot came back as "the current model
    // cannot see images" — even on a vision model.
    tools.set_vision_supported(
        opts.vision
            || profile.is_some_and(|p| p.vision)
            || crate::attachments::sees_images(discovery.as_ref(), &model),
    );
    trace("serve: tools ready");

    let gate = Arc::new(ServeGate {
        responders: Mutex::new(Vec::new()),
        counter: AtomicU64::new(0),
        timeout: Duration::from_secs(300),
    });
    // With --no-ask the permission layer never reaches the card gate: what it
    // would ask about is denied at once, so an unattended turn can never wait
    // five minutes for a button nobody will press. A tgbot profile needs no
    // flag: the users' room carries an allowlist gate (its own tools pass,
    // the rest is refused without a card — B1), and the dev room's bypass
    // never asks, so the deny-all gate only catches the blacklisted shell.
    let approval_gate: Arc<dyn flashagent_core::permissions::ApprovalGate> = if profile
        .is_some_and(|p| p.permission_mode == "planning")
    {
        Arc::new(flashagent_core::permissions::AllowlistGate::new(
            profile.unwrap().allowlist,
        ))
    } else if opts.no_ask || profile.is_some() {
        Arc::new(flashagent_core::permissions::DenyAllGate)
    } else {
        gate.clone()
    };
    let perm_state =
        Arc::new(flashagent_core::permissions::PermissionState::new(config.permission_mode, approval_gate));
    perm_state.set_project_root(cwd.clone());
    // The memory vault is the brain's own store, not a code change: a write
    // there must never wait on an approval card (the users' room runs manual).
    let mut rules = flashagent_core::permissions::RuleSet::default();
    rules.always_allow_tools.push("memory".into());
    perm_state.set_rules(rules);
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
            ask_user_available: true,
            goal_mode: false,
            detail,
            reasoning_supported,
            local: !remote,
            // B5: the room's language and tone, said once. The bot glues no
            // per-turn language lines any more — the prefix carries them.
            room_directive: profile.map(|p| p.language_directive.to_string()),
        }
        .with_personality(&config.personality),
    );
    trace("serve: prompt ready");

    // Mid-turn compaction for rooms with a bounded window: the TUI's
    // TurnCompactor over the session archive. Off unless the operator
    // asked for it with `--compact-at` — a serve the owner did not cap keeps
    // its whole history and never summarizes it behind his back. A tgbot
    // profile brings its own trigger (the users' room compacts at 200k, B3);
    // an explicit flag still wins.
    // `--compact-at` turns the trigger into an absolute token count; the
    // fixed estimate covers the system prompt (memory included), the schemas
    // ride within the room.
    let compact_at = opts.compact_at.or_else(|| profile.and_then(|p| p.compact_at));
    let compactor = if capacity > 0 && config.auto_compact_context && compact_at.is_some() {
        let fixed = flashagent_llm::count_tokens(&model, &system_prompt) + 4;
        let threshold_pct =
            threshold_pct_of(compact_at, config.context_compact_threshold, capacity);
        Some((capacity, fixed, threshold_pct))
    } else {
        None
    };

    let source_dyn: Arc<dyn flashagent_core::LlmSource> = source.clone();
    // The last session of every key lives next to the serve's cwd: a reboot
    // or an update resumes the rooms mid-conversation. An unreadable map is
    // an empty one — the rooms simply start fresh, as they did before.
    let session_map_path = cwd.join(".serve-sessions.json");
    let session_map = Arc::new(Mutex::new(
        std::fs::read_to_string(&session_map_path)
            .ok()
            .and_then(|text| {
                serde_json::from_str::<std::collections::HashMap<String, String>>(&text).ok()
            })
            .unwrap_or_default(),
    ));
    let base = ServeBase {
        source: source.clone(),
        source_dyn,
        perm,
        gate,
        questions,
        session_map,
        session_map_path,
        config: config.clone(),
        model: model.clone(),
        effort,
        cwd_display,
        system_prompt,
        compactor,
        profile,
    };

    // Sessions are created lazily: `main` exists from the start, every
    // /report discussion spawns its own on the first turn addressed to it.
    let sessions: Arc<Mutex<std::collections::HashMap<String, Arc<Session>>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));
    let main = session_for(&sessions, &base, "main");

    let addr = format!("127.0.0.1:{}", opts.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let token = std::env::var("FLASHAGENT_SERVE_TOKEN").ok().filter(|t| !t.trim().is_empty());
    println!("FlashAgent serve on http://{addr} (session {})", main.session_id);
    trace("serve: listening");
    let server_sessions = sessions.clone();
    let server_base = Arc::new(base);
    // A stop — an update, a reboot, an operator's restart — must not orphan
    // the answer being written. Stop taking requests, let the running turns
    // finish and save their session, then leave; the next process resumes
    // those conversations from disk.
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => {
                trace("serve: shutting down, draining turns");
                break;
            }
            accepted = listener.accept() => {
                let (mut stream, _) = accepted?;
                let sessions = server_sessions.clone();
                let base = server_base.clone();
                let token = token.clone();
                tokio::spawn(async move {
                    match read_request(&mut stream).await {
                        Ok(req) => handle(&mut stream, req, &sessions, &base, &token).await,
                        Err(_) => respond(&mut stream, 400, &serde_json::json!({"error": "bad request"})).await,
                    }
                });
            }
        }
    }
    drain_sessions(&sessions, SHUTDOWN_DRAIN).await;
    Ok(())
}

/// How long a stop waits for the turn in flight. It also bounds the drain
/// before systemd's TimeoutStopSec would kill the process anyway.
const SHUTDOWN_DRAIN: Duration = Duration::from_secs(60);

/// SIGTERM (systemd, a reboot) or SIGINT (Ctrl+C) asks for the drain above.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let (Ok(mut term), Ok(mut int)) =
            (signal(SignalKind::terminate()), signal(SignalKind::interrupt()))
        {
            tokio::select! {
                _ = term.recv() => {}
                _ = int.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

/// Waits until no session has a turn running, or the limit runs out. A turn
/// that outlives the limit is killed by systemd, exactly as before.
async fn drain_sessions(
    sessions: &Mutex<std::collections::HashMap<String, Arc<Session>>>,
    limit: Duration,
) {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        let running = {
            let map = sessions.lock().expect("sessions");
            map.values()
                .any(|s| s.state.lock().expect("serve state").running)
        };
        if !running || tokio::time::Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flashagent_core::permissions::{ApprovalGate, ApprovalRequest};

    #[tokio::test]
    async fn an_unanswered_approval_is_denied_when_the_clock_runs_out() {
        let state: SharedState = Arc::new(Mutex::new(ServeState::default()));
        let gate = ServeGate {
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
        let decision = CURRENT_SESSION
            .scope(state.clone(), async { gate.approve(&req).await })
            .await;
        assert!(matches!(decision, flashagent_core::permissions::Decision::Deny));
        // The feed says the wait ended, so the bot can tell the person why.
        let st = state.lock().unwrap();
        assert_eq!(st.pending, None);
        let shown = format!("{:?}", st.events);
        assert!(shown.contains("approval_timeout"), "{shown}");
    }

    #[tokio::test]
    async fn an_approval_answered_through_the_api_allows_the_call() {
        let state: SharedState = Arc::new(Mutex::new(ServeState::default()));
        let gate = Arc::new(ServeGate {
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
            let state = state.clone();
            tokio::spawn(async move {
                CURRENT_SESSION
                    .scope(state, async { gate.approve(&req).await })
                    .await
            })
        };
        // The pending approval shows in the feed with its diff.
        tokio::time::sleep(Duration::from_millis(60)).await;
        let (pending_tool, pending_diff) = {
            let st = state.lock().unwrap();
            let pending = st.pending.clone().expect("the approval is in the feed");
            (pending["tool"].clone(), pending["diff"].clone())
        };
        assert_eq!(pending_tool, "edit");
        assert_eq!(pending_diff, "+ hello");
        assert!(
            gate.answer_for(&state, true),
            "the answer lands on the waiting call"
        );
        assert!(matches!(waiter.await.unwrap(), flashagent_core::permissions::Decision::Allow));
    }

    #[tokio::test]
    async fn a_question_reaches_the_feed_and_the_answer_resolves_it() {
        use flashagent_tools::ask_user::QuestionGate as _;

        let state: SharedState = Arc::new(Mutex::new(ServeState::default()));
        let questions = Arc::new(ServeQuestions::with_timeout(Duration::from_secs(5)));
        let gate = questions.clone();
        let waiter = {
            let state = state.clone();
            tokio::spawn(async move {
                CURRENT_SESSION
                    .scope(state, async {
                        let options = vec!["Плеер".to_string(), "Очередь".to_string()];
                        gate.ask("Что чинить?", Some(&options), false, None).await
                    })
                    .await
            })
        };
        let mut ask = None;
        for _ in 0..50 {
            if let Some(found) = state
                .lock()
                .unwrap()
                .events
                .iter()
                .find(|e| e.get("type") == Some(&serde_json::json!("ask")))
                .cloned()
            {
                ask = Some(found);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let ask = ask.expect("the ask event is in the session feed");
        assert_eq!(ask["question"], "Что чинить?");
        assert_eq!(ask["options"][0], "Плеер");
        assert_eq!(ask["multi_select"], false);
        let id = ask["id"].as_u64().expect("an ask id");
        assert!(questions.answer_for(&state, id, "Плеер=2, Очередь=1".into()));
        let (answer, write_in) = waiter.await.unwrap().unwrap();
        assert_eq!(answer, "Плеер=2, Очередь=1");
        assert!(!write_in);
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
    fn a_steer_needs_a_running_turn() {
        let state: SharedState = Arc::new(Mutex::new(ServeState::default()));
        // Idle: the words have nowhere to land, and the bot says so to the room.
        let (status, body) =
            steer_answer(&state, r#"{"text":"стоп, не так"}"#.as_bytes());
        assert_eq!(status, 409);
        assert_eq!(body["error"], "no turn is running");
        // With a live channel the steer reaches the loop's receiver.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        state.lock().unwrap().steer_tx = Some(tx);
        let (status, _) =
            steer_answer(&state, r#"{"text":"стоп, не так"}"#.as_bytes());
        assert_eq!(status, 200);
        let steer = rx.try_recv().expect("the steer is delivered");
        assert_eq!(steer.text, "стоп, не так");
        // A body without text is a bad request, not a mystery.
        let (status, _) = steer_answer(&state, b"{}");
        assert_eq!(status, 400);
    }

    #[test]
    fn a_turn_while_a_turn_runs_is_a_steer_by_itself() {
        let state: SharedState = Arc::new(Mutex::new(ServeState::default()));
        // Idle: a prompt starts a turn.
        assert!(matches!(turn_path(&state, "го"), TurnPath::Start));
        // A prompt into a running turn flies in as a steer — the client just
        // posts /turn and never learns the difference.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        {
            let mut st = state.lock().unwrap();
            st.running = true;
            st.steer_tx = Some(tx);
        }
        assert!(matches!(turn_path(&state, "стоп, не так"), TurnPath::Steered));
        assert_eq!(rx.try_recv().expect("delivered").text, "стоп, не так");
        // A running turn with no channel left: busy, nothing to fly into.
        state.lock().unwrap().steer_tx = None;
        assert!(matches!(
            turn_path(&state, "го"),
            TurnPath::BusyWithoutChannel
        ));
    }

    #[test]
    fn a_chat_reply_call_becomes_a_reply_event() {
        // The delivery is the call's own ToolStarted event: the args carry
        // the target and the full text, per session — no shared outbox to
        // route wrong.
        let reply = chat_reply_event(r#"{"message_id": 42, "text": "держи"}"#).expect("delivered");
        assert_eq!(reply["type"], "reply");
        assert_eq!(reply["message_id"], 42);
        assert_eq!(reply["text"], "держи");
        // A call that names no message delivers nothing.
        assert!(chat_reply_event(r#"{"text": "без id"}"#).is_none());
        assert!(chat_reply_event("не json").is_none());
    }

    /// A minimal base for the session tests: real tools and permissions, no
    /// network — a session never calls the server from these paths.
    fn test_base() -> ServeBase {
        let tools = Arc::new(flashagent_tools::BuiltinTools::new(Default::default()).unwrap());
        let gate: Arc<ServeGate> = Arc::new(ServeGate {
            responders: Mutex::new(Vec::new()),
            counter: AtomicU64::new(0),
            timeout: Duration::from_secs(10),
        });
        let questions = Arc::new(ServeQuestions::with_timeout(Duration::from_secs(10)));
        let perm: &'static flashagent_core::permissions::PermissionedTools =
            Box::leak(Box::new(flashagent_core::permissions::PermissionedTools::new(
                tools.clone(),
                Some(tools.clone()),
                Arc::new(flashagent_core::permissions::PermissionState::new(
                    flashagent_core::PermissionMode::Manual,
                    gate.clone(),
                )),
            )));
        ServeBase {
            source: Arc::new(crate::backend::BackendSource(flashagent_llm::Client::new(
                flashagent_core::AppConfig::default().endpoint(),
                "",
            ))),
            source_dyn: {
                let src: Arc<dyn flashagent_core::LlmSource> = Arc::new(
                    crate::backend::BackendSource(flashagent_llm::Client::new(
                        flashagent_core::AppConfig::default().endpoint(),
                        "",
                    )),
                );
                src
            },
            perm,
            gate,
            questions,
            session_map: Arc::new(Mutex::new(std::collections::HashMap::new())),
            session_map_path: std::env::temp_dir().join("flashagent-serve-test-sessions.json"),
            config: flashagent_core::AppConfig::default(),
            model: "test-model".into(),
            effort: "off".into(),
            cwd_display: std::env::temp_dir().display().to_string(),
            system_prompt: "test prompt".into(),
            compactor: None,
            profile: None,
        }
    }

    #[test]
    fn the_session_key_comes_from_query_or_body() {
        assert_eq!(session_key_of("", &serde_json::json!({})), "main");
        assert_eq!(
            session_key_of("since=3&session=report-7", &serde_json::json!({})),
            "report-7"
        );
        assert_eq!(
            session_key_of("", &serde_json::json!({"session": "report-9"})),
            "report-9"
        );
        assert_eq!(session_key_of("session=", &serde_json::json!({})), "main");
    }

    #[test]
    fn sessions_are_reused_by_key_and_isolated_by_feed() {
        let base = test_base();
        let sessions: Mutex<std::collections::HashMap<String, Arc<Session>>> =
            Mutex::new(std::collections::HashMap::new());
        let main = session_for(&sessions, &base, "main");
        let main_again = session_for(&sessions, &base, "main");
        assert!(Arc::ptr_eq(&main, &main_again), "same key, same session");
        let report = session_for(&sessions, &base, "report-42");
        assert!(!Arc::ptr_eq(&main, &report), "a report is its own brain");
        // A feed entry in one session never shows in the other.
        report
            .state
            .lock()
            .unwrap()
            .events
            .push(serde_json::json!({"type": "turn_started"}));
        assert!(main.state.lock().unwrap().events.is_empty());
        assert_eq!(report.state.lock().unwrap().events.len(), 1);
        // Each session carries its own id, prefixed by its key.
        assert!(report.session_id.starts_with("report-42-"));
        assert_eq!(
            sessions.lock().unwrap().len(),
            2,
            "main + one report: no hidden sessions"
        );
    }

    #[test]
    fn a_restarted_serve_resumes_the_saved_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let history = vec![
            ChatMessage::system("старый промпт"),
            ChatMessage::user("вопрос"),
            ChatMessage::assistant("ответ"),
        ];
        crate::sessions::save_session_in(
            dir.path(),
            "main-session_1",
            "test-model",
            "/tmp",
            &history,
            &[],
            false,
            None,
        )
        .unwrap();
        // The turns come back, the system prompt is rebuilt from the current
        // one: AGENTS, memory and the tier may have changed since the save.
        let restored =
            load_serve_history(dir.path(), "main-session_1", "новый промпт").expect("restored");
        assert_eq!(restored.len(), 3);
        assert_eq!(restored[0].role, flashagent_llm::Role::System);
        assert_eq!(restored[0].content, "новый промпт");
        assert_eq!(restored[1].content, "вопрос");
        assert_eq!(restored[2].content, "ответ");
        // A missing session, or one that holds only its prompt, starts fresh.
        assert!(load_serve_history(dir.path(), "main-session_404", "x").is_none());
        crate::sessions::save_session_in(
            dir.path(),
            "main-session_2",
            "test-model",
            "/tmp",
            &[ChatMessage::system("только промпт")],
            &[],
            false,
            None,
        )
        .unwrap();
        assert!(load_serve_history(dir.path(), "main-session_2", "x").is_none());
    }

    #[tokio::test]
    async fn stopping_waits_for_the_running_turn() {
        let base = test_base();
        let sessions: Mutex<std::collections::HashMap<String, Arc<Session>>> =
            Mutex::new(std::collections::HashMap::new());
        let session = session_for(&sessions, &base, "main");
        session.state.lock().unwrap().running = true;
        let state = session.state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            state.lock().unwrap().running = false;
        });
        let started = std::time::Instant::now();
        drain_sessions(&sessions, Duration::from_secs(2)).await;
        assert!(
            started.elapsed() >= Duration::from_millis(40),
            "the drain did not wait for the turn"
        );
    }

    #[test]
    fn the_session_map_survives_and_forgets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".serve-sessions.json");
        let map = Mutex::new(std::collections::HashMap::new());
        remember_session(&map, &path, "main", "main-session_1");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("main-session_1"));
        // A throwaway service key is never remembered: its session is wiped.
        remember_session(&map, &path, "svc-abc", "svc-abc-session_2");
        assert!(map.lock().unwrap().get("svc-abc").is_none());
        // A reset forgets the key, so the next turn starts a fresh session.
        forget_session(&map, &path, "main");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("main-session_1"));
    }

    #[test]
    fn the_compaction_trigger_becomes_a_percentage_of_the_cap() {
        // 55k of 65k: the trigger sits just past 55k (84%).
        assert_eq!(threshold_pct_of(Some(55_000), 0, 65_000), 84);
        // An at beyond the cap falls back to the configured or default share.
        assert_eq!(
            threshold_pct_of(Some(70_000), 0, 65_000),
            flashagent_core::default_compact_threshold(65_000)
        );
        // No --compact-at: the config value, or the window's default at 0.
        assert_eq!(threshold_pct_of(None, 0, 65_000), 80);
        assert_eq!(threshold_pct_of(None, 90, 65_000), 90);
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

    #[test]
    fn the_feed_trims_its_head_but_keeps_an_absolute_cursor() {
        let mut st = ServeState::default();
        for i in 0..(FEED_RETAIN * 2 + 10) {
            st.push_event(serde_json::json!({"type": "n", "i": i}));
        }
        // The window is bounded even after a long-lived feed.
        assert!(st.events.len() <= FEED_RETAIN * 2);
        assert!(st.events_base > 0);
        let total = st.total_events();
        assert_eq!(st.events_base + st.events.len(), total);
        // A current cursor gets exactly the tail.
        let tail = st.events_since(total - 5);
        assert_eq!(tail.len(), 5);
        assert_eq!(tail[4]["i"], FEED_RETAIN * 2 + 9);
        // The cursor at total means "nothing new", not "send everything".
        assert!(st.events_since(total).is_empty());
        // A cursor older than the window still gets the retained window, so
        // a client that was away re-syncs instead of reading an empty feed.
        let stale = st.events_since(0);
        assert_eq!(stale.len(), st.events.len());
        assert!(st.total_events() >= stale.len());
    }
}
