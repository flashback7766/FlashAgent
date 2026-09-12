use super::*;

/// [`LlmSource`] over the OpenAI-compatible backend.
pub(crate) struct BackendSource(pub(crate) flashagent_llm::OpenAiCompat);

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
