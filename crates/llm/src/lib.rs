//! Backend trait and protocol adapters. Every tool-call wire format (native,
//! Hermes XML, `[TOOL_CALLS]`, bare JSON) is normalized into one [`ToolCall`];
//! JSON repair runs before parsing.

pub mod encoding;
pub mod repair;
pub mod types;
pub mod thinking;
pub mod tokenizer;

mod parse;
mod openai;

pub use encoding::base64_encode;
pub use openai::OpenAiCompat;
pub use parse::{ChunkParser, SseDecoder, TextToolScanner, ScannerEvent};
pub use repair::{effective_args, repair_json};
pub use thinking::{DiscoveredModel, ServerDiscovery, TaskComplexity, ThinkingProfile, ThinkingProtocol};
pub use tokenizer::count_tokens;
pub use types::{ChatMessage, FinishReason, LlmError, LlmEvent, MtpStats, Role, ThinkingEffort, ToolCall, ToolSpec, TurnOptions, Usage, estimate_tokens};

use async_trait::async_trait;
use futures::stream::BoxStream;

#[async_trait]
pub trait LlmBackend: Send + Sync {
    fn name(&self) -> &str;

    /// Tool results travel inside `messages` with [`Role::Tool`].
    async fn stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<BoxStream<'static, Result<LlmEvent, LlmError>>, LlmError>;

    async fn stream_with_options(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        _options: &TurnOptions,
    ) -> Result<BoxStream<'static, Result<LlmEvent, LlmError>>, LlmError> {
        self.stream(messages, tools).await
    }
}
