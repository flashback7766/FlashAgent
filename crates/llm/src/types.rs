//! Wire types shared by all backends, protocol-free.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Raw JSON text, parsed by the tool layer.
    pub args_json: String,
}

#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters_json: String,
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    pub reasoning: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    /// `data:` URLs. A model that cannot see images never receives them.
    pub images: Vec<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            reasoning: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
            images: Vec::new(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            reasoning: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
            images: Vec::new(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            reasoning: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
            images: Vec::new(),
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            reasoning: None,
            tool_call_id: Some(tool_call_id.into()),
            tool_calls: Vec::new(),
            images: Vec::new(),
        }
    }
}

/// Speculative decoding (MTP) draft stats.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MtpStats {
    pub total_draft_tokens: u64,
    pub accepted_draft_tokens: u64,
    pub rejected_draft_tokens: u64,
}

impl MtpStats {
    pub fn acceptance_rate(&self) -> f64 {
        if self.total_draft_tokens == 0 {
            0.0
        } else {
            (self.accepted_draft_tokens as f64 / self.total_draft_tokens as f64) * 100.0
        }
    }
}

/// `None` when the backend did not report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub prompt: Option<i64>,
    pub completion: Option<i64>,
    /// Prefix cache reuse (f_keep).
    pub cached: Option<i64>,
    pub mtp: Option<MtpStats>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    ToolUse,
    Length,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LlmEvent {
    TextDelta(String),
    ReasoningDelta(String),
    /// Concatenate `args_delta` per `index`.
    ToolCallDelta {
        index: usize,
        /// Present on the first fragment only.
        id: Option<String>,
        /// Present on the first fragment only.
        name: Option<String>,
        args_delta: String,
    },
    /// May arrive anywhere, usually at the end.
    Usage(Usage),
    Done(FinishReason),
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("backend returned {status}: {body}")]
    Status {
        status: u16,
        body: String,
    },
    #[error("stream interrupted: {0}")]
    Stream(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThinkingEffort {
    #[default]
    Auto,
    Default,
    Off,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnOptions {
    pub thinking: ThinkingEffort,
    /// Explicit preset name from the API ("xhigh", "on", "low").
    pub custom_effort: Option<String>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub repeat_penalty: Option<f32>,
    pub presence_penalty: Option<f32>,
    pub min_p: Option<f32>,
    pub max_tokens: Option<u32>,
}

/// Used when the backend sends no usage: ~4 chars per token for Latin text,
/// ~3.2 for Cyrillic.
pub fn estimate_tokens(text: &str) -> i64 {
    if text.is_empty() {
        return 0;
    }
    let latinish = text
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || c.is_ascii_whitespace())
        .count();
    let other = text.chars().count() - latinish;
    let tokens = (latinish as f64 / 4.0) + (other as f64 / 3.2);
    (tokens.ceil() as i64).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_is_monotonic_and_nonzero() {
        assert_eq!(estimate_tokens(""), 0);
        assert!(estimate_tokens("hello world") > 0);
        assert!(estimate_tokens("hello world friendship") > 0);
        assert!(estimate_tokens("a short") < estimate_tokens("a much longer sentence with words"));
    }
}
