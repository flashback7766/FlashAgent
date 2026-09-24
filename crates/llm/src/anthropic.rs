//! Anthropic's Messages API (`/v1/messages`).

use crate::client::{Client, EventStream};
use crate::thinking::ServerDiscovery;
use crate::types::{ChatMessage, LlmError, ToolSpec, TurnOptions};

pub(crate) async fn stream(
    _client: &Client,
    _messages: &[ChatMessage],
    _tools: &[ToolSpec],
    _options: &TurnOptions,
) -> Result<EventStream, LlmError> {
    Err(LlmError::Stream("the anthropic protocol is not implemented yet".into()))
}

pub(crate) async fn discover(_client: &Client) -> Option<ServerDiscovery> {
    None
}
