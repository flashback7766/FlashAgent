use super::*;
use flashagent_tui::i18n::{tr, tr_fill};

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct SavedToolCall {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) args_json: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct SavedMessage {
    pub(crate) role: String,
    pub(crate) content: String,
    pub(crate) reasoning: Option<String>,
    pub(crate) tool_call_id: Option<String>,
    pub(crate) tool_calls: Vec<SavedToolCall>,
    /// `data:` URLs. Without them a resumed model would answer about a picture it
    /// cannot see.
    #[serde(default)]
    pub(crate) images: Vec<String>,
    /// Signed thinking and the like, which the provider wants back verbatim
    /// when a resumed session goes on (see `ChatMessage::replay`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) replay: Option<serde_json::Value>,
}

impl From<&ChatMessage> for SavedMessage {
    fn from(m: &ChatMessage) -> Self {
        Self {
            role: m.role.as_str().to_string(),
            content: redact_secrets(&m.content),
            reasoning: m.reasoning.as_deref().map(redact_secrets),
            tool_call_id: m.tool_call_id.clone(),
            tool_calls: m
                .tool_calls
                .iter()
                .map(|tc| SavedToolCall {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    args_json: tc.args_json.clone(),
                })
                .collect(),
            images: m.images.clone(),
            replay: m.replay.clone(),
        }
    }
}

impl From<SavedMessage> for ChatMessage {
    fn from(m: SavedMessage) -> Self {
        let role = match m.role.as_str() {
            "system" => flashagent_llm::Role::System,
            "assistant" => flashagent_llm::Role::Assistant,
            "tool" => flashagent_llm::Role::Tool,
            _ => flashagent_llm::Role::User,
        };
        Self {
            role,
            content: m.content,
            reasoning: m.reasoning,
            tool_call_id: m.tool_call_id,
            tool_calls: m
                .tool_calls
                .into_iter()
                .map(|tc| flashagent_llm::ToolCall {
                    id: tc.id,
                    name: tc.name,
                    args_json: tc.args_json,
                })
                .collect(),
            images: m.images,
            replay: m.replay,
        }
    }
}

/// What a saved copy of a message shows instead of a key-shaped token.
///
/// Sessions and archives are the files users paste into a bug report, and a key
/// reaches them the ordinary way: the agent reads `~/.flashagent/config.json`
/// or a `.env` and the tool result is kept in the transcript. Losing the tail
/// of a token costs nothing; leaking one costs the account.
pub(crate) fn redact_secrets(text: &str) -> String {
    const PATTERNS: [&str; 8] = [
        "sk-ant-", "sk-proj-", "sk-or-v1-", "sk-", "AIza", "ghp_", "gho_", "xai-",
    ];
    let mut out = text.to_string();
    for prefix in PATTERNS {
        let mut from = 0;
        while let Some(rel) = out[from..].find(prefix) {
            let at = from + rel;
            // A token is the run of key characters after the prefix; everything
            // from the first one that cannot belong to a key is the user's text
            // again, so words like "sk-learn" or "sketch" are left alone.
            let end = out[at + prefix.len()..]
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '.')
                .map_or(out.len(), |i| at + prefix.len() + i);
            if end - at < prefix.len() + 8 {
                // Too short to be a key: ordinary prose that begins like one.
                from = at + prefix.len();
                continue;
            }
            out.replace_range(at..end, &format!("{prefix}[redacted]"));
            from = at + prefix.len() + "[redacted]".len();
        }
    }
    out
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct SavedSession {
    pub(crate) id: String,
    pub(crate) timestamp: u64,
    pub(crate) model: String,
    pub(crate) cwd: String,
    pub(crate) messages: Vec<SavedMessage>,
    /// What earlier compactions took out of the model's context, kept here so
    /// the session is one file: the exact wire messages of the turns that were
    /// summarized away, in the order they happened.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) archive: Vec<SavedMessage>,
    /// True while a turn was still running when this file was written.
    ///
    /// Without it the last message of a session interrupted by a reboot is
    /// indistinguishable from a finished answer: on resume the model reads half
    /// an answer as if it were the conclusion. Older files have no such field
    /// and are read as complete, which is what they were.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) turn_in_flight: bool,
    /// The name the model gave this conversation, for the picker. Older files
    /// have none and are listed under their first prompt, as they always were.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Word for the notice, so the reader and the writer cannot disagree on it.
pub(crate) const CUT_OFF_NOTICE: &str =
    "This session was interrupted while FlashAgent was answering. The last answer below may be cut off mid-sentence; ask again if it ends strangely.";

pub(crate) fn flashagent_home_dir() -> Option<std::path::PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(|h| std::path::PathBuf::from(h).join(".flashagent"))
}

/// To `~/.flashagent/sessions/<id>.json`, written beside the final name and
/// renamed, so a crash or a second instance never leaves half a file. Errors
/// are shown, not swallowed: a session the user believes is saved but is not
/// is found missing only when it is needed.
pub(crate) fn save_session_file(
    session_id: &str,
    model: &str,
    cwd: &str,
    history: &[ChatMessage],
    archive: &[ChatMessage],
    title: Option<&str>,
) -> Result<std::path::PathBuf, String> {
    save_session_file_marked(session_id, model, cwd, history, archive, false, title)
}

/// As [`save_session_file`], but records that the history ends inside a running
/// turn, so a later resume does not read the last message as a finished answer.
pub(crate) fn save_session_file_marked(
    session_id: &str,
    model: &str,
    cwd: &str,
    history: &[ChatMessage],
    archive: &[ChatMessage],
    turn_in_flight: bool,
    title: Option<&str>,
) -> Result<std::path::PathBuf, String> {
    let base_dir = sessions_dir().ok_or(flashagent_tui::i18n::tr("no home directory to save sessions in"))?;
    save_session_in(&base_dir, session_id, model, cwd, history, archive, turn_in_flight, title)
}

/// Earlier turns are removed from the model's live context after compaction.
/// Keep their exact wire messages in a separate, append-only transcript so the
/// summary can point the agent back to commands and user wording if needed.
/// Where the session's file is: the archive of the earlier turns lives in it,
/// so a session is one file and not a file with a companion.
pub(crate) fn session_path(session_id: &str) -> Option<std::path::PathBuf> {
    if !is_session_id(session_id) {
        return None;
    }
    Some(sessions_dir()?.join(format!("{session_id}.json")))
}


/// `turn_in_flight` says the history ends in the middle of a turn, which is
/// recorded in the file so a reader can tell a finished answer from half of one.
#[allow(clippy::too_many_arguments)]
pub(crate) fn save_session_in(
    base_dir: &std::path::Path,
    session_id: &str,
    model: &str,
    cwd: &str,
    history: &[ChatMessage],
    archive: &[ChatMessage],
    turn_in_flight: bool,
    title: Option<&str>,
) -> Result<std::path::PathBuf, String> {
    std::fs::create_dir_all(base_dir).map_err(|e| {
        tr_fill("cannot create {path}: {e}", &[("path", &base_dir.display().to_string()), ("e", &e.to_string())])
    })?;
    // The folder holds conversations: only the user should see what is in it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(base_dir, std::fs::Permissions::from_mode(0o700));
    }
    let path = base_dir.join(format!("{session_id}.json"));
    let saved = SavedSession {
        id: session_id.to_string(),
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        model: model.to_string(),
        cwd: cwd.to_string(),
        messages: history.iter().map(SavedMessage::from).collect(),
        archive: archive.iter().map(SavedMessage::from).collect(),
        turn_in_flight,
        title: title.map(str::to_string).filter(|t| !t.trim().is_empty()),
    };
    let data = serde_json::to_string_pretty(&saved)
        .map_err(|e| tr_fill("cannot encode the session: {e}", &[("e", &e.to_string())]))?;
    // The pid keeps two instances saving the same id from sharing a temp file.
    let tmp = base_dir.join(format!(".{session_id}.{}.tmp", std::process::id()));
    let written = write_private(&tmp, data.as_bytes());
    if let Err(e) = written.and_then(|_| std::fs::rename(&tmp, &path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(tr_fill("cannot write {path}: {e}", &[("path", &path.display().to_string()), ("e", &e.to_string())]));
    }
    // The rename itself is only durable once the folder says so: a crash after
    // it but before this can leave the previous save in place, which is how a
    // session loses the last turn to a machine that lost power.
    sync_dir(base_dir);
    Ok(path)
}

/// With the mode set at creation: a session holds the whole conversation,
/// including whatever the user pasted into it. The same helper writes the
/// config, so the two cannot drift apart.
use flashagent_core::paths::write_private;

#[cfg(unix)]
fn sync_dir(dir: &std::path::Path) {
    if let Ok(f) = std::fs::File::open(dir) {
        let _ = f.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_dir(_dir: &std::path::Path) {}

/// Nothing retries after it, so an unwritable sessions folder falls back to
/// the system temp folder, and the message names both.
pub(crate) fn save_on_exit(
    session_id: &str,
    model: &str,
    cwd: &str,
    history: &[ChatMessage],
    title: Option<&str>,
) -> Result<String, String> {
    let why = match save_session_file(session_id, model, cwd, history, &[], title) {
        Ok(_) => return Ok(session_id.to_string()),
        Err(why) => why,
    };
    let spare = std::env::temp_dir().join("flashagent-unsaved");
    match save_session_in(&spare, session_id, model, cwd, history, &[], false, title) {
        Ok(path) => Err(tr_fill(
            "{why}.\nA copy was written to {path} — move it into ~/.flashagent/sessions/ to resume it.",
            &[("why", &why), ("path", &path.display().to_string())],
        )),
        Err(also) => Err(tr_fill(
            "{why}; the spare copy failed too: {also}",
            &[("why", &why), ("also", &also)],
        )),
    }
}

/// As the resume picker lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionSummary {
    pub(crate) id: String,
    pub(crate) timestamp: u64,
    /// The first prompt, on one line.
    pub(crate) title: String,
    /// Prompts and answers, not tool traffic.
    pub(crate) messages: usize,
    /// For search in the picker.
    pub(crate) text: String,
}

pub(crate) fn sessions_dir() -> Option<std::path::PathBuf> {
    flashagent_home_dir().map(|home| home.join("sessions"))
}

/// No separators, no `..`: the id is also the save path, so it must not climb
/// out of the folder.
pub(crate) fn is_session_id(id: &str) -> bool {
    !id.is_empty() && !id.contains(['/', '\\']) && !id.contains("..")
}

/// Timestamp to the second, plus the pid: two launches in the same second
/// used to get the same id, and the second saved over the first.
pub(crate) fn new_session_id() -> String {
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    unused_session_id(sessions_dir().as_deref(), ts, std::process::id())
}

/// With a counter when the file exists (a pid reused in the same second).
fn unused_session_id(dir: Option<&std::path::Path>, ts: u64, pid: u32) -> String {
    let taken = |id: &str| dir.is_some_and(|d| d.join(format!("{id}.json")).exists());
    let base = format!("session_{ts}_{pid}");
    if !taken(&base) {
        return base;
    }
    (2..).map(|n| format!("{base}_{n}")).find(|id| !taken(id)).unwrap_or(base)
}

/// "just now", "5 min ago", "3 days ago".
pub(crate) fn ago(timestamp: u64, now: u64) -> String {
    let secs = now.saturating_sub(timestamp);
    match secs {
        0..=59 => tr("just now").to_string(),
        60..=3599 => tr_fill("{n} min ago", &[("n", &(secs / 60).to_string())]),
        3600..=86_399 => tr_fill("{n} h ago", &[("n", &(secs / 3600).to_string())]),
        86_400..=172_799 => tr("yesterday").to_string(),
        _ => tr_fill("{n} days ago", &[("n", &(secs / 86_400).to_string())]),
    }
}

/// How long a running turn waits between writes of the session file.
const AUTOSAVE_GAP: std::time::Duration = std::time::Duration::from_secs(5);

impl App {
    pub(crate) fn autosave(&mut self, session_id: &str, cwd_display: &str) {
        if !self.config.auto_save_sessions {
            return;
        }
        let model = self.current_model.clone();
        let err = save_session_file(session_id, &model, cwd_display, &self.history, &self.archive, self.session_title.as_deref()).err();
        if let Some(why) = err {
            self.notice(tr_fill(
                "Session not saved: {why} · retrying after the next turn and on exit",
                &[("why", &why)],
            ));
        }
        self.last_autosave = Some(std::time::Instant::now());
    }

    /// Puts a running turn on disk, but not on every step: the whole
    /// conversation is written each time, so doing it per step would be
    /// quadratic in the length of the turn. Five seconds bounds how much a
    /// crash can take away while leaving the writing flat.
    ///
    /// This is the difference between losing a turn and losing the last few
    /// seconds of it. Reported live: a machine that lost power mid-turn kept
    /// nothing of the turn, because the only write happened after it ended.
    pub(crate) fn autosave_during_turn(&mut self, cx: &LoopCtx<'_>) {
        if !self.config.auto_save_sessions {
            return;
        }
        if self.last_autosave.is_some_and(|last| last.elapsed() < AUTOSAVE_GAP) {
            return;
        }
        // The running turn's own history, not this app's copy of it. `self.history`
        // is only replaced when the turn ends, so writing that here wrote the
        // same pre-turn bytes every five seconds and lost the whole turn to a
        // crash — which is the thing this function exists to prevent.
        let Some(shared) = self.turn_history.clone() else { return };
        let snapshot = shared.snapshot();
        let model = self.current_model.clone();
        // The turn has not ended, so whatever the last message holds is a
        // snapshot of an unfinished answer. Written into the file, so a resume
        // knows not to read it as the conclusion.
        let err = save_session_file_marked(cx.session_id, &model, cx.cwd_display, &snapshot, &self.archive, true, self.session_title.as_deref()).err();
        if let Some(why) = err {
            self.notice(tr_fill(
                "Session not saved: {why} · retrying after the next turn and on exit",
                &[("why", &why)],
            ));
        }
        self.last_autosave = Some(std::time::Instant::now());
    }

    /// `--resume <id>` before the first frame. False, with the reason in the
    /// transcript, when the session could not be read.
    pub(crate) fn resume_at_start(&mut self, id: &str, memory_block: &str, perm: &PermissionedTools) -> bool {
        match sessions_dir()
            .ok_or_else(|| flashagent_tui::i18n::tr("No home directory to read sessions from; starting fresh.").to_string())
            .and_then(|dir| read_session(&dir, id))
        {
            Ok(saved) => {
                // The name travels with the conversation: a resumed session is
                // listed under the name it already had, not under its first
                // prompt again.
                self.session_title = saved.title.clone().filter(|t| !t.trim().is_empty());
                let restored = restore_session(saved, &mut self.chat, &mut self.history, &mut self.archive);
                strip_legacy_memory(&mut self.history, memory_block);
                update_context_usage(&self.current_model, &mut self.context_usage, &self.history, memory_block, &self.chat, perm);
                self.notice(tr_fill("Resumed session {id} · {messages}", &[("id", id), ("messages", &flashagent_tui::plural(restored, "message", "messages"))]));
                true
            }
            Err(why) => {
                self.chat.push_line(LineKind::ToolError, why);
                false
            }
        }
    }

    /// A session picked from the list replaces the open one on screen and in
    /// the history, which is saved first. False if nothing was switched.
    pub(crate) fn switch_session(&mut self, current: &str, id: &str, memory_block: &str, perm: &PermissionedTools) -> bool {
        // A recap still being written belongs to the session being left: its
        // suggestion and its name must not land in the one being opened.
        self.cancel_recap();
        if self.config.auto_save_sessions && worth_saving(&self.history) {
            if let Err(why) =
                save_session_file(current, &self.current_model, &self.cwd_display, &self.history, &self.archive, self.session_title.as_deref())
            {
                // Switching away would drop the only copy, the one in memory.
                self.notice(tr_fill(
                    "Not switching: this session could not be saved ({why})",
                    &[("why", &why)],
                ));
                return false;
            }
        }
        let saved = match sessions_dir()
            .ok_or_else(|| flashagent_tui::i18n::tr("No home directory to read sessions from").to_string())
            .and_then(|dir| read_session(&dir, id))
        {
            Ok(saved) => saved,
            Err(why) => {
                self.notice(why);
                return false;
            }
        };
        // The new session gets the prompt this one runs on now, memory
        // included, rather than whatever text happened to be in the old
        // system message.
        let base_system = build_system_prompt(
            &self
                .prompt_config
                .clone()
                .with_personality(&self.config.personality)
                .with_goal_mode(self.goal_active),
        );
        self.history = vec![ChatMessage::system(base_system)];
        self.chat.clear();
        // The name comes with the session: the one being opened is listed under
        // the name it had, or under none — never under the name the session
        // just left carried.
        self.session_title = saved.title.clone().filter(|t| !t.trim().is_empty());
        let restored = restore_session(saved, &mut self.chat, &mut self.history, &mut self.archive);
        strip_legacy_memory(&mut self.history, memory_block);
        self.latest_suggestion = None;
        // The bill belongs to the session that earned it. The status bar and
        // /cost read these, and carrying the old session's spend into the new
        // one charged a conversation for work it never did.
        self.cost = flashagent_core::CostLedger::default();
        self.token_tracker = TokenTracker::new(self.current_model.clone());
        update_context_usage(&self.current_model, &mut self.context_usage, &self.history, memory_block, &self.chat, perm);
        self.notice(tr_fill("Resumed session {id} · {messages}", &[("id", id), ("messages", &flashagent_tui::plural(restored, "message", "messages"))]));
        true
    }

    /// Leaves out the one already open.
    pub(crate) fn open_session_picker(&mut self, cwd_display: &str, current_session: &str) {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let items: Vec<SelectItem<String>> = sessions_dir()
            .map(|dir| sessions_in(&dir, cwd_display))
            .unwrap_or_default()
            .into_iter()
            // A fresh start has nothing to hide an older session behind.
            .filter(|s| s.id != current_session || !worth_saving(&self.history))
            .map(|s| {
                let title = if s.title.is_empty() { flashagent_tui::i18n::tr("(no prompt)").to_string() } else { s.title };
                // The count with its word: English has two forms, Russian three,
                // and the word follows the number in both.
                let messages = flashagent_tui::text::plural(s.messages, "message", "messages");
                SelectItem::with_description(title, format!("{} · {messages}", ago(s.timestamp, now)), s.id)
                    .with_search_text(s.text)
            })
            .collect();
        if items.is_empty() {
            self.custom_placeholder = Some(tr("No other saved session in this folder").to_string());
            self.suggested_prompt = None;
        } else {
            self.open_overlay(Overlay::Sessions(SelectMenu::new(flashagent_tui::i18n::tr("Resume a session"), items).with_noun(flashagent_tui::select::Noun::SESSIONS)));
        }
        self.renderer.request_reprint();
    }
}

/// Newest first. A broken file is skipped so it cannot hide the others.
pub(crate) fn sessions_in(dir: &std::path::Path, cwd: &str) -> Vec<SessionSummary> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<SessionSummary> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .filter_map(|content| serde_json::from_str::<SavedSession>(&content).ok())
        .filter(|s| s.cwd == cwd)
        .map(|s| {
            // The name the model gave the conversation; the first prompt is
            // what older files and sessions the model never named fall back to.
            let title = s
                .title
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    s.messages
                        .iter()
                        .find(|m| m.role == "user")
                        .map(|m| {
                            let prompt = extract_user_prompt(&m.content).lines().next().unwrap_or("").trim();
                            if prompt.is_empty() && !m.images.is_empty() {
                                "[image]".to_string()
                            } else {
                                prompt.to_string()
                            }
                        })
                        .unwrap_or_default()
                });
            let text = s
                .messages
                .iter()
                .filter(|m| m.role == "user" || m.role == "assistant")
                .map(|m| if m.role == "user" { extract_user_prompt(&m.content) } else { m.content.as_str() })
                .collect::<Vec<_>>()
                .join("\n");
            SessionSummary {
                text,
                messages: s.messages.iter().filter(|m| m.role == "user" || m.role == "assistant").count(),
                id: s.id,
                timestamp: s.timestamp,
                title: flashagent_tui::truncate_middle(&title, 70),
            }
        })
        .collect();
    found.sort_by(|a, b| b.timestamp.cmp(&a.timestamp).then_with(|| b.id.cmp(&a.id)));
    found
}

/// `--resume ../../somewhere` must not read outside `dir`.
pub(crate) fn read_session(dir: &std::path::Path, id: &str) -> Result<SavedSession, String> {
    if !is_session_id(id) {
        return Err(tr_fill("'{id}' is not a session id; starting a new session.", &[("id", id)]));
    }
    let path = dir.join(format!("{id}.json"));
    let content = std::fs::read_to_string(&path)
        .map_err(|_| tr_fill("No saved session '{id}' in ~/.flashagent/sessions; starting fresh.", &[("id", id)]))?;
    serde_json::from_str(&content).map_err(|_| {
        tr_fill(
            "Session '{id}' is unreadable, so a new session starts; the file is left as it was: {path}",
            &[("id", id), ("path", &path.display().to_string())],
        )
    })
}

/// After the system prompt `history` already starts with. Returns how many
/// messages came back.
/// The conversation is drawn again as it was: thoughts, answers, and a line
/// per tool call with its result. Only prompts and answer text came back, so
/// nothing on screen said which files were edited or which commands ran,
/// and the sentences between calls ran together into one paragraph.
pub(crate) fn restore_session(
    saved: SavedSession,
    chat: &mut ChatView,
    history: &mut Vec<ChatMessage>,
    archive: &mut Vec<ChatMessage>,
) -> usize {
    use flashagent_core::{DoneReason, LoopEvent};
    let before = history.len();
    // What earlier compactions took out of the context comes back with the
    // session: the file is the one place it is kept.
    archive.extend(saved.archive.into_iter().map(ChatMessage::from));
    if saved.turn_in_flight {
        // Said on screen and to the model: the answer underneath may stop
        // mid-word, and both of them would otherwise take it as finished.
        chat.push_system(CUT_OFF_NOTICE);
    }
    // A call is drawn when its result comes, as while it ran.
    let mut calls: std::collections::HashMap<String, flashagent_llm::ToolCall> = std::collections::HashMap::new();
    let mut in_turn = false;
    for saved_msg in saved.messages {
        let msg: ChatMessage = saved_msg.into();
        match msg.role {
            flashagent_llm::Role::User if flashagent_core::is_task_notice(&msg.content) => {
                for line in crate::tasks::notice_lines(&msg.content) {
                    chat.push_system(&line);
                }
                history.push(msg);
            }
            // A tool's picture has no line of its own; its tool line stands for it.
            flashagent_llm::Role::User if !flashagent_core::is_prompt(&msg) => history.push(msg),
            flashagent_llm::Role::User => {
                if std::mem::take(&mut in_turn) {
                    chat.on_event(&LoopEvent::Done(DoneReason::Completed));
                }
                let prompt = extract_user_prompt(&msg.content);
                if prompt.is_empty() && !msg.images.is_empty() {
                    chat.push_user("[image]");
                } else {
                    chat.push_user(prompt);
                }
                history.push(msg);
            }
            flashagent_llm::Role::Assistant => {
                in_turn = true;
                if let Some(reasoning) = msg.reasoning.as_deref().filter(|r| !r.trim().is_empty()) {
                    chat.on_event(&LoopEvent::ReasoningDelta(reasoning.to_string()));
                }
                if !msg.content.trim().is_empty() {
                    chat.on_event(&LoopEvent::TurnDelta(msg.content.clone()));
                }
                // The next message's words start a block of their own.
                chat.end_streaming();
                calls.extend(msg.tool_calls.iter().map(|c| (c.id.clone(), c.clone())));
                history.push(msg);
            }
            // The fresh system prompt wins; only a compaction summary carries over. A
            // second system message mid-history breaks strict templates (Gemma, Qwen).
            flashagent_llm::Role::System => {
                // The summary, in whichever shape the session was saved in: the
                // current one keeps it in the system message after the marker,
                // the older one had it as the whole message.
                let carried: Option<String> = match msg.content.find(COMPACTED_MARK) {
                    Some(pos) => Some(msg.content[pos + COMPACTED_MARK.len()..].trim_start().to_string()),
                    None => msg.content.strip_prefix(COMPACTED_INTRO).map(|s| s.trim_start().to_string()),
                };
                if let (Some(carried), Some(system)) = (carried, history.first_mut()) {
                    // The context line of the saved one is left out: the live
                    // one counts, and two of them would say two different
                    // things about the same window.
                    let carried = carried.split(crate::App::CONTEXT_LINE_MARK).next().unwrap_or(carried.as_str());
                    // Only the summary travels: the archive marker, the path to
                    // the archive and the card's token counts are the reader's,
                    // and the model read them as instructions of its own.
                    let carried = carried.split(ARCHIVE_MARK).next().unwrap_or(carried);
                    let carried = carried.split(crate::compact::COMPACTION_STATS_MARK).next().unwrap_or(carried);
                    let carried = carried.trim_end().to_string();
                    let block = format!("{COMPACTED_MARK}{carried}");
                    // The summary goes before the live context line, not after
                    // it: the first turn rewrites that line and truncates from
                    // its marker, and a summary parked behind it was dropped
                    // before the model ever saw it. Reported live on a resumed
                    // session, which answered as if the summary never existed.
                    match system.content.find(crate::App::CONTEXT_LINE_MARK) {
                        Some(at) => system.content.insert_str(at, &block),
                        None => system.content.push_str(&block),
                    }
                }
                // The transcript starts where the summary does: the turns it
                // replaced are gone, and a reader who resumes should see why.
                if let Some(summary) = crate::compact::summary_from_system(&msg.content) {
                    let stats = crate::compact::stats_from_system(&msg.content);
                    chat.push_restored_compaction_card(
                        summary,
                        stats.map(|(input, _)| input),
                        stats.map(|(_, output)| output),
                    );
                }
            }
            flashagent_llm::Role::Tool => {
                if let Some(call) = msg.tool_call_id.as_ref().and_then(|id| calls.remove(id)) {
                    chat.on_event(&LoopEvent::ToolStarted { id: call.id.clone(), name: call.name.clone(), args_json: call.args_json.clone() });
                    chat.on_event(&LoopEvent::ToolFinished {
                        id: call.id,
                        is_error: result_was_an_error(&msg.content),
                        result_len: msg.content.chars().count(),
                        result: Some(msg.content.clone()),
                    });
                }
                history.push(msg);
            }
        }
    }
    if in_turn {
        chat.on_event(&LoopEvent::Done(DoneReason::Completed));
    }
    // How long each thought took was not saved.
    chat.forget_thought_times();
    history.len() - before
}

/// A stored result says nothing else about how the call ended: the tools and
/// the loop write these words when one fails or does not run.
fn result_was_an_error(content: &str) -> bool {
    content.starts_with("error:")
        || content.starts_with(flashagent_tui::i18n::tr("not executed:"))
        || content.starts_with(flashagent_core::permissions::DENIED)
        || content == flashagent_core::permissions::DECLINED
        || flashagent_core::stopped_by_user(content)
}

/// `history` always opens with the system prompt, so an emptiness check would
/// save a start-and-quit and offer a `--resume` that restores nothing.
pub(crate) fn worth_saving(history: &[ChatMessage]) -> bool {
    history.iter().any(|m| m.role == flashagent_llm::Role::User)
}

pub(crate) fn extract_user_prompt(content: &str) -> &str {
    // Older builds put the memory block in front of the first user message.
    // The separator belongs to that block, not to the user's own text.
    let content = if content.starts_with("# Memory (automatically loaded") {
        content.split_once("\n\n---\n\n").map(|(_, user_part)| user_part).unwrap_or("")
    } else {
        content
    };
    // Show the goal the user typed, not the directive scaffolding.
    if content.starts_with("[AUTONOMOUS GOAL DIRECTIVE]") {
        if let Some(rest) = content.split_once("Target goal:") {
            let goal = rest.1.lines().next().unwrap_or("").trim();
            if !goal.is_empty() {
                return goal;
            }
        }
    }
    content.trim()
}

/// Older builds put memory in front of the first user message. The system
/// prompt now carries it, so drop the old copy from a restored history.
fn strip_legacy_memory(history: &mut [ChatMessage], memory_block: &str) {
    if memory_block.is_empty() {
        return;
    }
    let Some(first) = history.iter_mut().find(|m| m.role == flashagent_llm::Role::User) else { return };
    if let Some(rest) = first.content.strip_prefix(memory_block) {
        first.content = rest.strip_prefix("\n\n---\n\n").unwrap_or(rest).to_string();
    } else if first.content.starts_with("# Memory (automatically loaded") {
        // The memory file changed since the session was saved; the wrapper is
        // still the old one, so use its separator.
        first.content = first
            .content
            .split_once("\n\n---\n\n")
            .map(|(_, user_part)| user_part)
            .unwrap_or("")
            .to_string();
    }
}




#[cfg(test)]
mod session_tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn the_session_with_its_archive_is_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = save_session_in(
            dir.path(),
            "session_test",
            "m",
            "~/p",
            &[ChatMessage::user("private conversation")],
            &[ChatMessage::user("archived words")],
            false,
            None,
        )
        .unwrap();
        assert_eq!(std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        // The archive of the earlier turns is in the session's own file.
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("archived words"), "the archive is not in the session: {saved}");
    }

    #[test]
    fn an_old_memory_prefix_is_removed_from_a_restored_prompt() {
        let memory = "# Memory (automatically loaded)\n\nuse tabs";
        let mut history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user(format!("{memory}\n\n---\n\nfix the parser")),
        ];
        strip_legacy_memory(&mut history, memory);
        assert_eq!(history[1].content, "fix the parser");
    }

    #[test]
    fn a_memory_wrapped_goal_still_shows_the_goal() {
        let content = "# Memory (automatically loaded)\n\nrules\n\n---\n\n[AUTONOMOUS GOAL DIRECTIVE]\nTarget goal: Create hello.txt\n\nRules";
        assert_eq!(extract_user_prompt(content), "Create hello.txt");
    }

    fn write(dir: &std::path::Path, id: &str, timestamp: u64, cwd: &str, first_prompt: &str) {
        let saved = SavedSession {
            archive: Vec::new(),
            id: id.to_string(),
            timestamp,
            model: "m".into(),
            cwd: cwd.to_string(),
            messages: vec![
                SavedMessage::from(&ChatMessage::system("sys")),
                SavedMessage::from(&ChatMessage::user(first_prompt)),
                SavedMessage::from(&ChatMessage::assistant("answer")),
            ],
            turn_in_flight: false,
            title: None,
        };
        std::fs::write(dir.join(format!("{id}.json")), serde_json::to_string(&saved).unwrap()).unwrap();
    }

    #[test]
    fn only_this_folders_sessions_are_listed_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "session_100", 100, "~/proj", "first question");
        write(dir.path(), "session_300", 300, "~/proj", "latest question\nsecond line");
        write(dir.path(), "session_200", 200, "~/other", "elsewhere");
        std::fs::write(dir.path().join("broken.json"), "not json").unwrap();

        let listed = sessions_in(dir.path(), "~/proj");
        let ids: Vec<&str> = listed.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["session_300", "session_100"], "a broken file or another folder's session must not show up");
        assert_eq!(listed[0].title, "latest question", "one line of the first prompt");
        assert_eq!(listed[0].messages, 2, "prompts and answers, not the system prompt");
        assert!(sessions_in(dir.path(), "~/nowhere").is_empty());
        assert!(sessions_in(&dir.path().join("missing"), "~/proj").is_empty());
    }

    #[test]
    fn a_new_session_never_takes_the_id_of_one_already_saved() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(unused_session_id(Some(dir.path()), 500, 7), "session_500_7");
        write(dir.path(), "session_500_7", 500, "~/proj", "first");
        assert_eq!(unused_session_id(Some(dir.path()), 500, 7), "session_500_7_2");
        write(dir.path(), "session_500_7_2", 500, "~/proj", "second");
        assert_eq!(unused_session_id(Some(dir.path()), 500, 7), "session_500_7_3");
        assert_eq!(unused_session_id(None, 500, 7), "session_500_7");
    }

    #[test]
    fn the_name_travels_through_a_save_and_back() {
        let dir = tempfile::tempdir().unwrap();
        let history = vec![ChatMessage::system("sys"), ChatMessage::user("fix it"), ChatMessage::assistant("done")];
        save_session_in(dir.path(), "s", "m", "~/p", &history, &[], false, Some("Fixing the store parser")).unwrap();
        let saved = read_session(dir.path(), "s").unwrap();
        assert_eq!(saved.title.as_deref(), Some("Fixing the store parser"));

        // A name that is only spaces is not a name: the file is written without
        // the field at all, so the picker falls back to the first prompt.
        save_session_in(dir.path(), "s2", "m", "~/p", &history, &[], false, Some("   ")).unwrap();
        let raw = std::fs::read_to_string(dir.path().join("s2.json")).unwrap();
        assert!(!raw.contains("title"), "an empty name is noise in a file a user may read: {raw}");
        assert_eq!(read_session(dir.path(), "s2").unwrap().title, None);

        // And a save without a name stays without one.
        save_session_in(dir.path(), "s3", "m", "~/p", &history, &[], false, None).unwrap();
        assert_eq!(read_session(dir.path(), "s3").unwrap().title, None);
    }

    #[test]
    fn the_picker_prefers_the_name_the_model_gave() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "session_100", 100, "~/proj", "fix the login bug");
        let path = dir.path().join("session_100.json");

        let listed = sessions_in(dir.path(), "~/proj");
        assert_eq!(listed[0].title, "fix the login bug", "an unnamed session is listed under its first prompt");

        let mut saved: SavedSession = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        saved.title = Some("Fixing the store parser".into());
        std::fs::write(&path, serde_json::to_string(&saved).unwrap()).unwrap();
        let listed = sessions_in(dir.path(), "~/proj");
        assert_eq!(listed[0].title, "Fixing the store parser", "the model's name, not the first prompt");

        // A name that is only spaces is not a name either.
        saved.title = Some("   ".into());
        std::fs::write(&path, serde_json::to_string(&saved).unwrap()).unwrap();
        let listed = sessions_in(dir.path(), "~/proj");
        assert_eq!(listed[0].title, "fix the login bug");
    }

    #[test]
    fn two_instances_started_in_the_same_second_get_different_ids() {
        // Neither has saved yet, so only the pid tells them apart.
        let dir = tempfile::tempdir().unwrap();
        assert_ne!(unused_session_id(Some(dir.path()), 500, 7), unused_session_id(Some(dir.path()), 500, 8));
    }

    #[test]
    fn saving_replaces_the_file_whole_and_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let history = vec![ChatMessage::system("sys"), ChatMessage::user("first"), ChatMessage::assistant("one")];
        let path = save_session_in(dir.path(), "s", "m", "~/p", &history, &[], false, None).unwrap();
        let mut longer = history.clone();
        longer.push(ChatMessage::user("second"));
        save_session_in(dir.path(), "s", "m", "~/p", &longer, &[], false, None).unwrap();

        let saved: SavedSession = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved.messages.len(), 4);
        let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(files.len(), 1, "a temporary file was left behind: {files:?}");
    }

    #[test]
    fn a_save_that_cannot_happen_says_why() {
        let dir = tempfile::tempdir().unwrap();
        // A file where the folder should be.
        let blocked = dir.path().join("sessions");
        std::fs::write(&blocked, "").unwrap();
        let err = save_session_in(&blocked, "s", "m", "~/p", &[ChatMessage::user("hi")], &[], false, None).unwrap_err();
        assert!(err.contains("sessions"), "{err}");
    }

    #[test]
    fn an_interrupted_turn_is_marked_so_a_resume_does_not_read_it_as_an_answer() {
        let dir = tempfile::tempdir().unwrap();
        // A snapshot taken while the model was still writing.
        let history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("write the parser"),
            ChatMessage::assistant("Sure. First I will open the fi"),
        ];
        let path = save_session_in(dir.path(), "s", "m", "~/p", &history, &[], true, None).unwrap();
        let saved: SavedSession = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(saved.turn_in_flight, "the file does not say the answer is unfinished");

        let mut chat = ChatView::default();
        let mut history = vec![ChatMessage::system("new prompt")];
        let count = restore_session(saved, &mut chat, &mut history, &mut Vec::new());
        let shown = chat.render(100).iter().map(|(_, t)| flashagent_tui::strip_ansi(t)).collect::<Vec<_>>().join("\n");
        assert!(shown.contains("interrupted"), "nobody is told the answer stops mid-word:\n{shown}");
        assert!(shown.contains("First I will open the fi"), "the answer itself must still be there:\n{shown}");
        // The system message is replaced by the fresh one, so prompt + answer.
        assert_eq!(count, 2, "the unfinished answer stays in the history, so the model can go on from it");
        assert!(history.last().unwrap().content.ends_with("the fi"), "the answer was rewritten: {:?}", history.last().unwrap().content);
    }

    #[test]
    fn a_finished_turn_says_nothing_about_being_cut_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = save_session_in(dir.path(), "s", "m", "~/p", &[ChatMessage::user("hi"), ChatMessage::assistant("done")], &[], false, None).unwrap();
        let saved: SavedSession = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(!saved.turn_in_flight);
        assert!(!std::fs::read_to_string(&path).unwrap().contains("turn_in_flight"), "a whole session needs no marker: it is not noise in the file a user may read");
        let mut chat = ChatView::default();
        let mut history = vec![ChatMessage::system("sys")];
        restore_session(saved, &mut chat, &mut history, &mut Vec::new());
        let shown = chat.render(100).iter().map(|(_, t)| flashagent_tui::strip_ansi(t)).collect::<Vec<_>>().join("\n");
        assert!(!shown.contains("interrupted"), "a whole turn was called cut off:\n{shown}");
    }

    /// The files written before this field existed have to keep loading, or every
    /// session anyone already had becomes unresumable.
    #[test]
    fn a_session_file_written_before_the_marker_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        // Exactly the shape an earlier build wrote: no `turn_in_flight`.
        let old = r#"{
  "id": "session_1_1",
  "timestamp": 1700000000,
  "model": "m",
  "cwd": "~/proj",
  "messages": [
    { "role": "system", "content": "sys", "reasoning": null, "tool_call_id": null, "tool_calls": [], "images": [] },
    { "role": "user", "content": "the question", "reasoning": null, "tool_call_id": null, "tool_calls": [], "images": [] },
    { "role": "assistant", "content": "the answer", "reasoning": null, "tool_call_id": null, "tool_calls": [], "images": [] }
  ]
}"#;
        std::fs::write(dir.path().join("session_1_1.json"), old).unwrap();
        let saved = read_session(dir.path(), "session_1_1").expect("a pre-change session still loads");
        assert_eq!(saved.messages.len(), 3);
        assert!(!saved.turn_in_flight, "no marker means the old, whole reading");

        let mut chat = ChatView::default();
        let mut history = vec![ChatMessage::system("new prompt")];
        assert_eq!(restore_session(saved, &mut chat, &mut history, &mut Vec::new()), 2);
        let shown = chat.render(100).iter().map(|(_, t)| flashagent_tui::strip_ansi(t)).collect::<Vec<_>>().join("\n");
        assert!(shown.contains("the answer"), "the answer is gone:\n{shown}");
        assert!(!shown.contains("interrupted"), "an old file was called cut off:\n{shown}");

        // And the picker still lists it.
        let listed = sessions_in(dir.path(), "~/proj");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "the question");
    }

    #[cfg(unix)]
    #[test]
    fn a_session_file_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        // A file already there with open permissions must not keep them.
        let path = dir.path().join("s.json");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        save_session_in(dir.path(), "s", "m", "~/p", &[ChatMessage::user("hi")], &[], false, None).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn a_key_pasted_into_a_conversation_is_not_written_to_the_session_file() {
        let dir = tempfile::tempdir().unwrap();
        // What happens when the agent reads the config or a .env: the key comes
        // back as a tool result and is kept in the transcript.
        let tool = ChatMessage::tool_result("c1", "ANTHROPIC_API_KEY=sk-ant-api03-AAAABBBBCCCCDDDDEEEE1234\n");
        let user = ChatMessage::user("my key is sk-ant-api03-9999888877776666555544443333, keep it safe");
        let path = save_session_in(dir.path(), "s", "m", "~/p", &[user, tool], &[], false, None).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("sk-ant-api03-AAAABBBB"), "the tool result kept the key:\n{text}");
        assert!(!text.contains("sk-ant-api03-99998888"), "the prompt kept the key:\n{text}");
        assert!(text.contains("sk-ant-[redacted]"), "nothing marks what was taken out:\n{text}");
        assert!(text.contains("keep it safe"), "the user's own words are not the secret:\n{text}");
    }

    #[test]
    fn redaction_leaves_ordinary_words_that_begin_like_a_key_alone() {
        assert_eq!(redact_secrets("I read the sk-learn docs and sketched a parser"), "I read the sk-learn docs and sketched a parser");
        assert_eq!(redact_secrets("see https://example.com/sketch.png"), "see https://example.com/sketch.png");
        assert_eq!(redact_secrets("key = sk-short"), "key = sk-short");
        assert_eq!(
            redact_secrets("two: sk-ant-api03-AAAAAAAAAAAAAAAAAAAA and sk-proj-bbbbbbbbbbbbbbbbbbbb"),
            "two: sk-ant-[redacted] and sk-proj-[redacted]"
        );
        assert_eq!(redact_secrets("no secrets here"), "no secrets here");
        // Nothing is invented for text that has none.
        assert!(!redact_secrets("plain").contains("redacted"));
    }

    #[test]
    fn the_picker_says_how_long_ago_in_words() {
        assert_eq!(ago(1000, 1030), "just now");
        assert_eq!(ago(1000, 1000 + 5 * 60), "5 min ago");
        assert_eq!(ago(0, 3 * 3600), "3 h ago");
        assert_eq!(ago(0, 90_000), "yesterday");
        assert_eq!(ago(0, 4 * 86_400), "4 days ago");
        assert_eq!(ago(5000, 10), "just now", "a clock that moved back is not the future");
    }

    #[test]
    fn a_session_id_cannot_reach_outside_the_sessions_folder() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("sessions");
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        write(&dir, "session_1", 1, "~/proj", "hi");
        // Real session files outside the folder: without the id check they would be read.
        write(home.path(), "escaped", 1, "~/proj", "outside");
        write(&dir.join("inner"), "nested", 1, "~/proj", "nested");
        assert!(read_session(&dir, "session_1").is_ok());
        for bad in ["../escaped", "inner/nested", "inner\\nested", ".."] {
            let err = read_session(&dir, bad).err().unwrap_or_else(|| panic!("{bad:?} was accepted"));
            assert!(err.contains("not a session id"), "{bad:?}: {err}");
        }
        assert!(read_session(&dir, "").is_err());
        let dir = dir.as_path();
        let missing = read_session(dir, "session_404").err().expect("a missing session is an error");
        assert!(missing.contains("No saved session"), "{missing}");
    }

    #[test]
    fn restoring_keeps_the_new_system_prompt_and_only_a_summary_from_the_old_one() {
        let saved = SavedSession {
            archive: Vec::new(),
            id: "s".into(),
            timestamp: 1,
            model: "m".into(),
            cwd: "~/proj".into(),
            messages: vec![
                SavedMessage::from(&ChatMessage::system(format!("old prompt{COMPACTED_MARK}what happened before"))),
                SavedMessage::from(&ChatMessage::user("what is 2+2?")),
                SavedMessage::from(&ChatMessage::assistant("4")),
            ],
            turn_in_flight: false,
            title: None,
        };
        let mut chat = ChatView::default();
        let mut history = vec![ChatMessage::system("new prompt")];
        let restored = restore_session(saved, &mut chat, &mut history, &mut Vec::new());
        assert_eq!(restored, 2);
        assert_eq!(history.len(), 3);
        assert!(history[0].content.starts_with("new prompt"), "{}", history[0].content);
        assert!(!history[0].content.contains("old prompt"), "{}", history[0].content);
        assert!(history[0].content.contains("what happened before"), "{}", history[0].content);
        assert_eq!(chat.user_turn_count(), 1);
    }

    #[test]
    fn a_restored_summary_lands_before_the_context_line_so_the_next_turn_keeps_it() {
        // Reported live: after `-c`, the model answered as if the summary had
        // never been written. The startup context line comes before the system
        // message is restored, and the first turn rewrites that line from its
        // marker, which truncated everything after it -- the summary included.
        let saved = SavedSession {
            archive: Vec::new(),
            id: "s".into(),
            timestamp: 1,
            model: "m".into(),
            cwd: "~/proj".into(),
            messages: vec![
                SavedMessage::from(&ChatMessage::system(format!("old prompt{COMPACTED_MARK}what happened before"))),
                SavedMessage::from(&ChatMessage::user("and now?")),
                SavedMessage::from(&ChatMessage::assistant("carrying on")),
            ],
            turn_in_flight: false,
            title: None,
        };
        let mut chat = ChatView::default();
        let mut history = vec![ChatMessage::system("new prompt\n\n[Context] 100 of 1,000 tokens used, 900 left.")];
        restore_session(saved, &mut chat, &mut history, &mut Vec::new());
        let content = &history[0].content;
        let summary_at = content.find("what happened before").expect("the summary is kept");
        let context_at = content.find("[Context]").expect("the context line is kept");
        assert!(summary_at < context_at, "the next turn would cut the summary off: {content}");
        // What the first turn does to that line: cut from its marker and write
        // a fresh one. The summary must survive it.
        let cut = content.find(crate::App::CONTEXT_LINE_MARK).expect("the marker is where the line starts");
        let rewritten = format!("{}{}", &content[..cut], "\n\n[Context] 200 of 1,000 tokens used, 800 left.");
        assert!(rewritten.contains("what happened before"), "{rewritten}");
    }

    #[test]
    fn a_session_from_the_old_shape_still_carries_its_summary() {
        // The summary used to be its own system message. Without this it was
        // lost on a resume: the model answered as if it had never been written,
        // and the chat showed no compaction at all.
        let saved = SavedSession {
            archive: Vec::new(),
            id: "s".into(),
            timestamp: 1,
            model: "m".into(),
            cwd: "~/proj".into(),
            messages: vec![
                SavedMessage::from(&ChatMessage::system(format!("{COMPACTED_INTRO}what happened before"))),
                SavedMessage::from(&ChatMessage::user("and now?")),
                SavedMessage::from(&ChatMessage::assistant("carrying on")),
            ],
            turn_in_flight: false,
            title: None,
        };
        let mut chat = ChatView::default();
        let mut history = vec![ChatMessage::system("new prompt")];
        restore_session(saved, &mut chat, &mut history, &mut Vec::new());
        assert!(
            history[0].content.contains("what happened before"),
            "the summary is lost: {}",
            history[0].content
        );
        let lines: Vec<String> = chat.render(80).into_iter().map(|(_, t)| t).collect();
        assert!(
            lines.iter().any(|l| l.contains("Session compaction started")),
            "the chat shows no compaction: {lines:#?}"
        );
    }

    #[test]
    fn a_resumed_session_shows_its_tool_calls_and_thoughts_again() {
        let mut asked = ChatMessage::assistant("Reading it.");
        asked.reasoning = Some("**Reading Files**\nstore.py holds the bug".into());
        asked.tool_calls = vec![
            flashagent_llm::ToolCall { id: "c1".into(), name: "read_file".into(), args_json: r#"{"header":"Reading store.py","path":"store.py"}"#.into() },
            flashagent_llm::ToolCall { id: "c2".into(), name: "edit_file".into(), args_json: r#"{"header":"Fixing total_value","path":"store.py","edits":[{"old_string":"a","new_string":"b"}]}"#.into() },
        ];
        let saved = SavedSession {
            archive: Vec::new(),
            id: "s".into(),
            timestamp: 1,
            model: "m".into(),
            cwd: "~/proj".into(),
            messages: vec![
                SavedMessage::from(&ChatMessage::system("sys")),
                SavedMessage::from(&ChatMessage::user("fix the test")),
                SavedMessage::from(&asked),
                SavedMessage::from(&ChatMessage::tool_result("c1", "     1\tdef total_value():")),
                SavedMessage::from(&ChatMessage::tool_result("c2", flashagent_core::permissions::DECLINED)),
                SavedMessage::from(&ChatMessage::assistant("You declined the fix.")),
                SavedMessage::from(&ChatMessage::user("thanks")),
                SavedMessage::from(&ChatMessage::assistant("Any time.")),
            ],
            turn_in_flight: false,
            title: None,
        };
        let mut chat = ChatView::default();
        let mut history = vec![ChatMessage::system("new prompt")];
        restore_session(saved, &mut chat, &mut history, &mut Vec::new());
        // As the screen shows it, folded.
        let (settled, live) = chat.render_split(100, flashagent_tui::ReasoningExpansion::default());
        let text = settled.iter().chain(live.iter()).map(|(_, t)| flashagent_tui::strip_ansi(t)).collect::<Vec<_>>().join("\n");
        assert!(text.contains("Reading store.py"), "the read is gone:\n{text}");
        assert!(text.lines().any(|l| l.contains("Fixing total_value") && l.contains("· declined")), "the declined edit is gone:\n{text}");
        assert!(text.contains("Thought: Reading Files") && !text.contains("(1s)"), "{text}");
        // The words between calls are their own blocks, not one paragraph.
        let reading = text.find("Reading it.").unwrap();
        let declined = text.find("You declined the fix.").unwrap();
        assert!(text[reading..declined].contains("Reading store.py"), "{text}");
        assert!(!text.contains('▌'), "a restored answer is not still being written");
        assert_eq!(chat.user_turn_count(), 2);
    }

    #[test]
    fn restoring_an_image_only_prompt_shows_image_placeholder() {
        let mut user_msg = ChatMessage::user("");
        user_msg.images.push("data:image/png;base64,AAAA".to_string());
        let saved = SavedSession {
            archive: Vec::new(),
            id: "s_img".into(),
            timestamp: 1,
            model: "m".into(),
            cwd: "~/proj".into(),
            messages: vec![
                SavedMessage::from(&ChatMessage::system("sys")),
                SavedMessage::from(&user_msg),
                SavedMessage::from(&ChatMessage::assistant("I see an image")),
            ],
            turn_in_flight: false,
            title: None,
        };
        let mut chat = ChatView::default();
        let mut history = vec![ChatMessage::system("new prompt")];
        let restored = restore_session(saved, &mut chat, &mut history, &mut Vec::new());
        assert_eq!(restored, 2);
        assert_eq!(history[1].content, "");
        assert_eq!(history[1].images.len(), 1);
        let rendered = chat.render(100);
        assert!(rendered.iter().any(|(kind, text)| *kind == LineKind::User && text.contains("[image]")));
    }
}

