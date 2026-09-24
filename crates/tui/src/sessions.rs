use super::*;

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
            content: m.content.clone(),
            reasoning: m.reasoning.clone(),
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

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct SavedSession {
    pub(crate) id: String,
    pub(crate) timestamp: u64,
    pub(crate) model: String,
    pub(crate) cwd: String,
    pub(crate) messages: Vec<SavedMessage>,
}

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
pub(crate) fn save_session_file(session_id: &str, model: &str, cwd: &str, history: &[ChatMessage]) -> Result<std::path::PathBuf, String> {
    let base_dir = sessions_dir().ok_or("no home directory to save sessions in")?;
    save_session_in(&base_dir, session_id, model, cwd, history)
}

/// Earlier turns are removed from the model's live context after compaction.
/// Keep their exact wire messages in a separate, append-only transcript so the
/// summary can point the agent back to commands and user wording if needed.
pub(crate) fn compaction_archive_path(session_id: &str) -> Option<std::path::PathBuf> {
    if !is_session_id(session_id) {
        return None;
    }
    Some(sessions_dir()?.join("compactions").join(format!("{session_id}.jsonl")))
}

pub(crate) fn append_compaction_archive(path: &std::path::Path, messages: &[ChatMessage]) -> Result<(), String> {
    use std::io::Write;
    let mut chunk = Vec::new();
    for message in messages {
        serde_json::to_writer(&mut chunk, &SavedMessage::from(message))
            .map_err(|e| format!("cannot encode compaction archive: {e}"))?;
        chunk.push(b'\n');
    }
    let parent = path.parent().ok_or("compaction archive has no parent directory")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot protect {}: {e}", parent.display()))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("cannot protect {}: {e}", path.display()))?;
    }
    let previous_len = file.metadata().map_err(|e| format!("cannot inspect {}: {e}", path.display()))?.len();
    if let Err(e) = file.write_all(&chunk).and_then(|_| file.sync_all()) {
        let _ = file.set_len(previous_len);
        let _ = file.sync_all();
        return Err(format!("cannot write {}: {e}", path.display()));
    }
    Ok(())
}

pub(crate) fn save_session_in(
    base_dir: &std::path::Path,
    session_id: &str,
    model: &str,
    cwd: &str,
    history: &[ChatMessage],
) -> Result<std::path::PathBuf, String> {
    std::fs::create_dir_all(base_dir).map_err(|e| format!("cannot create {}: {e}", base_dir.display()))?;
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
    };
    let data = serde_json::to_string_pretty(&saved).map_err(|e| format!("cannot encode the session: {e}"))?;
    // The pid keeps two instances saving the same id from sharing a temp file.
    let tmp = base_dir.join(format!(".{session_id}.{}.tmp", std::process::id()));
    let written = std::fs::File::create(&tmp).and_then(|mut f| {
        use std::io::Write;
        f.write_all(data.as_bytes())?;
        f.sync_all()
    });
    if let Err(e) = written.and_then(|_| std::fs::rename(&tmp, &path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot write {}: {e}", path.display()));
    }
    Ok(path)
}

/// Nothing retries after it, so an unwritable sessions folder falls back to
/// the system temp folder, and the message names both.
pub(crate) fn save_on_exit(session_id: &str, model: &str, cwd: &str, history: &[ChatMessage]) -> Result<String, String> {
    let why = match save_session_file(session_id, model, cwd, history) {
        Ok(_) => return Ok(session_id.to_string()),
        Err(why) => why,
    };
    let spare = std::env::temp_dir().join("flashagent-unsaved");
    match save_session_in(&spare, session_id, model, cwd, history) {
        Ok(path) => Err(format!(
            "{why}.\nA copy was written to {} — move it into ~/.flashagent/sessions/ to resume it.",
            path.display()
        )),
        Err(also) => Err(format!("{why}; the spare copy failed too: {also}")),
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
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        86_400..=172_799 => "yesterday".to_string(),
        _ => format!("{} days ago", secs / 86_400),
    }
}

impl App {
    /// A failure is shown with when it will be retried; the next turn and quitting both retry.
    pub(crate) fn autosave(&mut self, session_id: &str, cwd_display: &str) {
        if !self.config.auto_save_sessions {
            return;
        }
        if let Err(why) = save_session_file(session_id, &self.current_model, cwd_display, &self.history) {
            self.notice(format!("Session not saved: {why} · retrying after the next turn and on exit"));
        }
    }

    /// `--resume <id>` before the first frame. False, with the reason in the
    /// transcript, when the session could not be read.
    pub(crate) fn resume_at_start(&mut self, id: &str, memory_block: &str, perm: &PermissionedTools) -> bool {
        match sessions_dir()
            .ok_or_else(|| "No home directory to read sessions from; starting fresh.".to_string())
            .and_then(|dir| read_session(&dir, id))
        {
            Ok(saved) => {
                let restored = restore_session(saved, &mut self.chat, &mut self.history);
                update_context_usage(&mut self.context_usage, &self.history, memory_block, &self.chat, perm);
                self.notice(format!("Resumed session {id} · {}", flashagent_tui::plural(restored, "message", "messages")));
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
        if self.config.auto_save_sessions && worth_saving(&self.history) {
            if let Err(why) = save_session_file(current, &self.current_model, &self.cwd_display, &self.history) {
                // Switching away would drop the only copy, the one in memory.
                self.notice(format!("Not switching: this session could not be saved ({why})"));
                return false;
            }
        }
        let saved = match sessions_dir()
            .ok_or_else(|| "No home directory to read sessions from".to_string())
            .and_then(|dir| read_session(&dir, id))
        {
            Ok(saved) => saved,
            Err(why) => {
                self.notice(why);
                return false;
            }
        };
        let base_system = self
            .history
            .first()
            .map(|m| m.content.split(COMPACTED_MARK).next().unwrap_or_default().to_string())
            .unwrap_or_default();
        self.history = vec![ChatMessage::system(base_system)];
        self.chat.clear();
        let restored = restore_session(saved, &mut self.chat, &mut self.history);
        self.latest_suggestion = None;
        update_context_usage(&mut self.context_usage, &self.history, memory_block, &self.chat, perm);
        self.notice(format!("Resumed session {id} · {}", flashagent_tui::plural(restored, "message", "messages")));
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
                let title = if s.title.is_empty() { "(no prompt)".to_string() } else { s.title };
                SelectItem::with_description(title, format!("{} · {} messages", ago(s.timestamp, now), s.messages), s.id)
                    .with_search_text(s.text)
            })
            .collect();
        if items.is_empty() {
            self.custom_placeholder = Some("No other saved session in this folder".to_string());
            self.suggested_prompt = None;
        } else {
            self.open_overlay(Overlay::Sessions(SelectMenu::new("Resume a session", items).with_noun("sessions")));
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
            let title = s
                .messages
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
                .unwrap_or_default();
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
        return Err(format!("'{id}' is not a session id; starting a new session."));
    }
    let path = dir.join(format!("{id}.json"));
    let content = std::fs::read_to_string(&path)
        .map_err(|_| format!("No saved session '{id}' in ~/.flashagent/sessions; starting fresh."))?;
    serde_json::from_str(&content).map_err(|_| {
        format!("Session '{id}' is unreadable, so a new session starts; the file is left as it was: {}", path.display())
    })
}

/// After the system prompt `history` already starts with. Returns how many
/// messages came back.
pub(crate) fn restore_session(saved: SavedSession, chat: &mut ChatView, history: &mut Vec<ChatMessage>) -> usize {
    let before = history.len();
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
                let prompt = extract_user_prompt(&msg.content);
                if prompt.is_empty() && !msg.images.is_empty() {
                    chat.push_user("[image]");
                } else {
                    chat.push_user(prompt);
                }
                history.push(msg);
            }
            flashagent_llm::Role::Assistant => {
                if !msg.content.trim().is_empty() {
                    chat.push_assistant(&msg.content);
                }
                history.push(msg);
            }
            // The fresh system prompt wins; only a compaction summary carries over. A
            // second system message mid-history breaks strict templates (Gemma, Qwen).
            flashagent_llm::Role::System => {
                // Older sessions stored the summary as its own system message.
                let title = COMPACTED_MARK.trim_start();
                if let (Some(pos), Some(system)) = (msg.content.find(title), history.first_mut()) {
                    system.content.push_str(COMPACTED_MARK);
                    system.content.push_str(msg.content[pos + title.len()..].trim_start());
                }
            }
            flashagent_llm::Role::Tool => history.push(msg),
        }
    }
    history.len() - before
}

/// `history` always opens with the system prompt, so an emptiness check would
/// save a start-and-quit and offer a `--resume` that restores nothing.
pub(crate) fn worth_saving(history: &[ChatMessage]) -> bool {
    history.iter().any(|m| m.role == flashagent_llm::Role::User)
}

pub(crate) fn extract_user_prompt(content: &str) -> &str {
    // Show the goal the user typed, not the directive scaffolding.
    if content.starts_with("[AUTONOMOUS GOAL DIRECTIVE]") {
        if let Some(rest) = content.split_once("Target goal:") {
            let goal = rest.1.lines().next().unwrap_or("").trim();
            if !goal.is_empty() {
                return goal;
            }
        }
    }
    if let Some((_mem, user_part)) = content.rsplit_once("\n\n---\n\n") {
        user_part.trim()
    } else {
        content.trim()
    }
}




/// Strict templates (Gemma, Mistral) require alternating turns; a turn with no
/// reply would leave two user messages in a row.
pub(crate) fn close_dangling_user(history: &mut Vec<ChatMessage>, note: &str) {
    if history.last().is_some_and(|m| m.role == flashagent_llm::Role::User) {
        history.push(ChatMessage::assistant(note));
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn compaction_archive_is_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("compactions").join("session_test.jsonl");
        append_compaction_archive(&archive, &[ChatMessage::user("private conversation")]).unwrap();
        assert_eq!(std::fs::metadata(archive.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(&archive).unwrap().permissions().mode() & 0o777, 0o600);
    }

    fn write(dir: &std::path::Path, id: &str, timestamp: u64, cwd: &str, first_prompt: &str) {
        let saved = SavedSession {
            id: id.to_string(),
            timestamp,
            model: "m".into(),
            cwd: cwd.to_string(),
            messages: vec![
                SavedMessage::from(&ChatMessage::system("sys")),
                SavedMessage::from(&ChatMessage::user(first_prompt)),
                SavedMessage::from(&ChatMessage::assistant("answer")),
            ],
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
    fn two_instances_started_in_the_same_second_get_different_ids() {
        // Neither has saved yet, so only the pid tells them apart.
        let dir = tempfile::tempdir().unwrap();
        assert_ne!(unused_session_id(Some(dir.path()), 500, 7), unused_session_id(Some(dir.path()), 500, 8));
    }

    #[test]
    fn saving_replaces_the_file_whole_and_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let history = vec![ChatMessage::system("sys"), ChatMessage::user("first"), ChatMessage::assistant("one")];
        let path = save_session_in(dir.path(), "s", "m", "~/p", &history).unwrap();
        let mut longer = history.clone();
        longer.push(ChatMessage::user("second"));
        save_session_in(dir.path(), "s", "m", "~/p", &longer).unwrap();

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
        let err = save_session_in(&blocked, "s", "m", "~/p", &[ChatMessage::user("hi")]).unwrap_err();
        assert!(err.contains("sessions"), "{err}");
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
            id: "s".into(),
            timestamp: 1,
            model: "m".into(),
            cwd: "~/proj".into(),
            messages: vec![
                SavedMessage::from(&ChatMessage::system(format!("old prompt{COMPACTED_MARK}what happened before"))),
                SavedMessage::from(&ChatMessage::user("what is 2+2?")),
                SavedMessage::from(&ChatMessage::assistant("4")),
            ],
        };
        let mut chat = ChatView::default();
        let mut history = vec![ChatMessage::system("new prompt")];
        let restored = restore_session(saved, &mut chat, &mut history);
        assert_eq!(restored, 2);
        assert_eq!(history.len(), 3);
        assert!(history[0].content.starts_with("new prompt"), "{}", history[0].content);
        assert!(!history[0].content.contains("old prompt"), "{}", history[0].content);
        assert!(history[0].content.contains("what happened before"), "{}", history[0].content);
        assert_eq!(chat.user_turn_count(), 1);
    }

    #[test]
    fn restoring_an_image_only_prompt_shows_image_placeholder() {
        let mut user_msg = ChatMessage::user("");
        user_msg.images.push("data:image/png;base64,AAAA".to_string());
        let saved = SavedSession {
            id: "s_img".into(),
            timestamp: 1,
            model: "m".into(),
            cwd: "~/proj".into(),
            messages: vec![
                SavedMessage::from(&ChatMessage::system("sys")),
                SavedMessage::from(&user_msg),
                SavedMessage::from(&ChatMessage::assistant("I see an image")),
            ],
        };
        let mut chat = ChatView::default();
        let mut history = vec![ChatMessage::system("new prompt")];
        let restored = restore_session(saved, &mut chat, &mut history);
        assert_eq!(restored, 2);
        assert_eq!(history[1].content, "");
        assert_eq!(history[1].images.len(), 1);
        let rendered = chat.render(100);
        assert!(rendered.iter().any(|(kind, text)| *kind == LineKind::User && text.contains("[image]")));
    }
}
