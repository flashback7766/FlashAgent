//! flashagent-llm: backend trait + protocol adapters.
//!
//! One internal [`ToolCall`] shape; every wire format (native tool calls,
//! Hermes-style XML tags, `[TOOL_CALLS]`, bare JSON) is normalized by parsers.
//! JSON repair runs before parsing so slightly-broken model output still lands.
//! Reasoning streams are first-class citizens.

pub mod repair;
pub mod types;
pub mod thinking;
pub mod tokenizer;

mod parse;
mod openai;

pub use openai::OpenAiCompat;
pub use parse::{ChunkParser, SseDecoder, TextToolScanner, ScannerEvent};
pub use repair::repair_json;
pub use thinking::{DiscoveredModel, ServerDiscovery, TaskComplexity, ThinkingProfile, ThinkingProtocol, analyze_turn_complexity, is_greeting_text};
pub use tokenizer::count_tokens;
pub use types::{ChatMessage, FinishReason, LlmError, LlmEvent, MtpStats, Role, ThinkingEffort, ToolCall, ToolSpec, TurnOptions, Usage, estimate_tokens};

use async_trait::async_trait;
use futures::stream::BoxStream;

/// A chat-completions backend. Streams normalized [`LlmEvent`]s.
#[async_trait]
pub trait LlmBackend: Send + Sync {
    /// Backend display name (used in UI and logs).
    fn name(&self) -> &str;

    /// Run one chat completion and stream its events.
    ///
    /// `tools` are advertised to the model; tool *results* travel inside
    /// `messages` with [`Role::Tool`].
    async fn stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<BoxStream<'static, Result<LlmEvent, LlmError>>, LlmError>;

    /// Run one chat completion with custom [`TurnOptions`] (e.g. reduced thinking budget).
    async fn stream_with_options(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        _options: &TurnOptions,
    ) -> Result<BoxStream<'static, Result<LlmEvent, LlmError>>, LlmError> {
        self.stream(messages, tools).await
    }
}
