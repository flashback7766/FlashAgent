//! Wire types shared by all backends. Deliberately protocol-free.

/// Message author role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// System prompt (conversation-level instructions; first message).
    System,
    /// User input.
    User,
    /// Model output.
    Assistant,
    /// Tool result fed back to the model.
    Tool,
}

impl Role {
    /// Wire name for the OpenAI-compatible protocol.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

/// A tool call produced by the model (normalized across protocols).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// Model-assigned call id, if the backend provides one.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// Arguments as JSON text (raw; parsed by the tool layer).
    pub args_json: String,
}

/// A tool advertised to the model.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    /// Tool name.
    pub name: String,
    /// Human/model-readable description.
    pub description: String,
    /// JSON Schema of arguments.
    pub parameters_json: String,
}

/// One conversation message.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    /// Author role.
    pub role: Role,
    /// Visible text content.
    pub content: String,
    /// Reasoning/thinking text (assistant messages only).
    pub reasoning: Option<String>,
    /// For [`Role::Tool`]: id of the call this result answers.
    pub tool_call_id: Option<String>,
    /// For assistant messages: calls made by the model in this message.
    pub tool_calls: Vec<ToolCall>,
    /// Images attached to this message, as `data:` URLs. A model that cannot
    /// see them simply never receives them.
    pub images: Vec<String>,
}

impl ChatMessage {
    /// System prompt (conversation-level instructions).
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

    /// Plain user message.
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

    /// Plain assistant message.
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

    /// Tool result answering `tool_call_id`.
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

/// Speculative decoding / Multi-Token Prediction (MTP) draft stats.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MtpStats {
    /// Total draft candidate tokens evaluated.
    pub total_draft_tokens: u64,
    /// Draft tokens accepted by the target model.
    pub accepted_draft_tokens: u64,
    /// Draft tokens rejected.
    pub rejected_draft_tokens: u64,
}

impl MtpStats {
    /// Acceptance rate as percentage (0.0 to 100.0).
    pub fn acceptance_rate(&self) -> f64 {
        if self.total_draft_tokens == 0 {
            0.0
        } else {
            (self.accepted_draft_tokens as f64 / self.total_draft_tokens as f64) * 100.0
        }
    }
}

/// Token accounting; `None` when the backend did not report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// Prompt tokens.
    pub prompt: Option<i64>,
    /// Completion tokens.
    pub completion: Option<i64>,
    /// Cached prompt tokens (prefix cache reuse / f_keep).
    pub cached: Option<i64>,
    /// Multi-Token Prediction / speculative decoding stats (if reported by backend).
    pub mtp: Option<MtpStats>,
}

/// Why the model stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    /// Natural end of turn.
    Stop,
    /// Model wants tool calls executed.
    ToolUse,
    /// Hit the context/length limit.
    Length,
}

/// Normalized stream event. All backends emit exactly these.
#[derive(Debug, Clone, PartialEq)]
pub enum LlmEvent {
    /// Piece of visible text.
    TextDelta(String),
    /// Piece of reasoning/thinking content.
    ReasoningDelta(String),
    /// Fragment of a tool call; concatenate `args_delta` per `index`.
    ToolCallDelta {
        /// Positional index of the call within the message.
        index: usize,
        /// Call id, present on the first fragment.
        id: Option<String>,
        /// Tool name, present on the first fragment.
        name: Option<String>,
        /// Raw JSON argument fragment.
        args_delta: String,
    },
    /// Token usage report (may arrive anywhere; usually the end).
    Usage(Usage),
    /// Stream finished.
    Done(FinishReason),
}

/// Backend errors.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// HTTP transport failure.
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    /// Server returned a non-2xx status.
    #[error("backend returned {status}: {body}")]
    Status {
        /// HTTP status code.
        status: u16,
        /// Response body (often an error JSON).
        body: String,
    },
    /// Stream broke mid-flight (network drop, malformed SSE).
    #[error("stream interrupted: {0}")]
    Stream(String),
    /// Refused by a policy or configuration setting.
    #[error("forbidden: {0}")]
    Forbidden(String),
}

/// Thinking / reasoning budget or effort level requested from the model.
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

/// Request options for a model completion turn.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnOptions {
    /// Desired thinking effort (e.g. low/off during stall recovery).
    pub thinking: ThinkingEffort,
    /// Explicit effort preset name from the API (e.g. "xhigh", "on", "low").
    pub custom_effort: Option<String>,
    /// Generation temperature.
    pub temperature: Option<f32>,
    /// Top P sampling.
    pub top_p: Option<f32>,
    /// Top K sampling.
    pub top_k: Option<u32>,
    /// Repeat penalty.
    pub repeat_penalty: Option<f32>,
    /// Presence penalty.
    pub presence_penalty: Option<f32>,
    /// Min P sampling.
    pub min_p: Option<f32>,
    /// Maximum completion tokens to generate.
    pub max_tokens: Option<u32>,
}

/// Rough local token estimate when the backend sends no usage.
/// ~4 chars per token for latin text; Cyrillic runs cheaper (~3.2).
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
