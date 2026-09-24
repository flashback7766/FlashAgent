use super::*;

pub(crate) struct BackendSource(pub(crate) flashagent_llm::Client);

impl BackendSource {
    pub(crate) fn profile(&self) -> Option<flashagent_llm::thinking::ThinkingProfile> {
        self.0.profile()
    }

    pub(crate) fn discovery(&self) -> Option<flashagent_llm::ServerDiscovery> {
        self.0.discovery()
    }

    pub(crate) fn set_model(&self, model: impl Into<String>) {
        self.0.set_model(model);
    }

    pub(crate) fn set_effort_bias(&self, steps: i8) {
        self.0.set_effort_bias(steps);
    }

    pub(crate) async fn measure_image_cost(&self, probe: &[u8], w: u32, h: u32) -> Option<(f32, f32)> {
        self.0.measure_image_cost(probe, w, h).await
    }

    pub(crate) async fn discover_server(&self) -> Option<flashagent_llm::ServerDiscovery> {
        self.0.discover_server().await
    }
}

#[async_trait::async_trait]
impl LlmSource for BackendSource {
    async fn turn(
        &self,
        messages: &[ChatMessage],
        tools: &[flashagent_llm::ToolSpec],
    ) -> Result<futures::stream::BoxStream<'static, Result<flashagent_llm::LlmEvent, flashagent_llm::LlmError>>, flashagent_llm::LlmError>
    {
        self.0.stream(messages, tools).await
    }

    async fn turn_with_options(
        &self,
        messages: &[ChatMessage],
        tools: &[flashagent_llm::ToolSpec],
        options: &flashagent_llm::TurnOptions,
    ) -> Result<futures::stream::BoxStream<'static, Result<flashagent_llm::LlmEvent, flashagent_llm::LlmError>>, flashagent_llm::LlmError>
    {
        self.0.stream_with_options(messages, tools, options).await
    }
}

/// Each look is up to two requests to a server that may be a laptop.
pub(crate) const SERVER_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// Never while a turn or any other model request (like the recap) is running,
/// and never twice at once.
pub(crate) fn should_poll_server(turn_running: bool, requests_in_flight: usize, already_polling: bool) -> bool {
    !turn_running && requests_in_flight == 0 && !already_polling
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_is_looked_at_only_when_nothing_is_happening() {
        assert!(should_poll_server(false, 0, false));
    }

    #[test]
    fn the_server_is_left_alone_while_the_agent_works() {
        assert!(!should_poll_server(true, 0, false), "a turn is running");
        assert!(!should_poll_server(false, 1, false), "a recap or a probe is still talking to the model");
        assert!(!should_poll_server(true, 2, false));
        assert!(!should_poll_server(false, 0, true), "the last look has not come back");
    }
}
