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
    /// Pictures sent with this message, as `data:` URLs. A resumed session
    /// that dropped them would leave the model answering about something it
    /// can no longer see.
    #[serde(default)]
    pub(crate) images: Vec<String>,
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

pub(crate) fn save_session_file(session_id: &str, model: &str, cwd: &str, history: &[ChatMessage]) -> Option<std::path::PathBuf> {
    let base_dir = flashagent_home_dir()?.join("sessions");
    let _ = std::fs::create_dir_all(&base_dir);
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
    if let Ok(data) = serde_json::to_string_pretty(&saved) {
        if std::fs::write(&path, data).is_ok() {
            return Some(path);
        }
    }
    None
}

/// Whether this run produced a conversation worth writing to disk.
///
/// `history` is never empty — it opens with the system prompt — so anything
/// that only checks emptiness saves a session for a start-and-quit, and hands
/// the user a `--resume` id that restores nothing.
pub(crate) fn worth_saving(history: &[ChatMessage]) -> bool {
    history.iter().any(|m| m.role == flashagent_llm::Role::User)
}

pub(crate) fn extract_user_prompt(content: &str) -> &str {
    // A resumed goal used to show its whole scaffolding as if the user had
    // typed it: "[AUTONOMOUS GOAL DIRECTIVE] You are operating in fully...".
    // What they typed was the goal.
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




/// Strict chat templates (Gemma, Mistral) demand alternating user/assistant
/// turns; a turn that ended before any reply would leave two user messages
/// in a row once the next prompt is sent.
pub(crate) fn close_dangling_user(history: &mut Vec<ChatMessage>, note: &str) {
    if history.last().is_some_and(|m| m.role == flashagent_llm::Role::User) {
        history.push(ChatMessage::assistant(note));
    }
}
