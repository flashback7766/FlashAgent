//! Warms the server's prompt cache before the first message. The first request
//! has nothing cached: system prompt, tool schemas and memory are read from
//! scratch (34 s for 8.3k tokens on LM Studio with Gemma 4 E2B, 0.9 s once
//! cached). So the prefix is sent with a one-token answer while the user types:
//! at start, after `--resume`, after a switch of model, provider or voice. It is assembled
//! the way the agent loop assembles a turn, so the cached tokens match. Only
//! for a server on this machine or network: a hosted API bills the warm-up and
//! reads a prompt in a second anyway.

use super::*;
use flashagent_core::ToolExec as _;
use std::hash::{Hash, Hasher};

/// System prompt, voice example, history, and the opening of the next user message.
pub(crate) fn warm_messages(history: &[ChatMessage], prelude: &[ChatMessage], memory_block: &str) -> Vec<ChatMessage> {
    let after_system = usize::from(history.first().is_some_and(|m| m.role == flashagent_llm::Role::System));
    let mut messages: Vec<ChatMessage> = history[..after_system]
        .iter()
        .chain(prelude)
        .chain(&history[after_system..])
        .cloned()
        .collect();
    let first = !history.iter().any(|m| m.role == flashagent_llm::Role::User);
    // The memory block goes in front of the first prompt, so it is part of the prefix.
    let opening = if first && !memory_block.is_empty() {
        format!("{memory_block}\n\n---\n\n")
    } else {
        "hi".to_string()
    };
    messages.push(ChatMessage::user(opening));
    messages
}

/// A model server on this machine or the local network. Anthropic and Gemini
/// are never local; an OpenAI-compatible server is local by what it runs
/// (LM Studio, llama.cpp) or by its address (vLLM on a LAN box).
pub(crate) fn worth_warming(endpoint: &flashagent_llm::Endpoint, kind: Option<flashagent_llm::thinking::ServerKind>) -> bool {
    use flashagent_llm::ApiProtocol;
    let local_address = || flashagent_core::url_host(&endpoint.url).is_some_and(|host| flashagent_core::is_local_host(&host));
    match endpoint.protocol {
        ApiProtocol::Anthropic | ApiProtocol::Gemini => false,
        ApiProtocol::Ollama => true,
        ApiProtocol::OpenAi => kind.is_some_and(|k| k.runs_local_models()) || local_address(),
    }
}

/// So the same prefix is not sent twice to the same server: after a switch
/// of provider it is another server's cache, even for a model of the same name.
fn prefix_key(server: &str, model: &str, messages: &[ChatMessage], specs: &[flashagent_llm::ToolSpec]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (server, model).hash(&mut h);
    for m in messages {
        (m.role as u8).hash(&mut h);
        m.content.hash(&mut h);
        m.reasoning.hash(&mut h);
        m.tool_call_id.hash(&mut h);
        for c in &m.tool_calls {
            (&c.id, &c.name, &c.args_json).hash(&mut h);
        }
        m.images.len().hash(&mut h);
    }
    for s in specs {
        (&s.name, &s.description, &s.parameters_json).hash(&mut h);
    }
    h.finish()
}

impl App {
    fn warm_key(&self, server: &str, perm: &'static PermissionedTools, memory_block: &str) -> (u64, Vec<ChatMessage>, Vec<flashagent_llm::ToolSpec>) {
        let messages = warm_messages(&self.history, &self.config.personality.voice_prelude(), memory_block);
        let specs = perm.specs();
        (prefix_key(server, &self.current_model, &messages, &specs), messages, specs)
    }

    /// Skipped when already sent or a turn has just read it.
    pub(crate) fn warm_prompt_cache(&mut self, source: &Arc<BackendSource>, perm: &'static PermissionedTools, memory_block: &str) {
        if self.running || self.current_model.is_empty() || !worth_warming(&source.0.endpoint(), source.discovery().map(|d| d.kind)) {
            return;
        }
        if let Some((_, task)) = self.warm_task.as_mut() {
            if !task.is_finished() {
                return;
            }
            // A failed warm-up (server not up yet) is retried.
            let failed = !matches!(futures::FutureExt::now_or_never(task), Some(Ok(true)));
            let key = self.warm_task.take().map(|(key, _)| key);
            if failed && self.cache_warm_key == key {
                self.cache_warm_key = None;
            }
        }
        let (key, messages, specs) = self.warm_key(&source.0.base_url(), perm, memory_block);
        if self.cache_warm_key == Some(key) {
            return;
        }
        self.cache_warm_key = Some(key);
        let mut opts = build_turn_options(&self.config, &self.current_effort);
        opts.max_tokens = Some(1);
        // Under "auto" the thinking switch depends on the unwritten prompt, and some
        // templates put it in the system turn; the warm-up assumes a task.
        if opts.thinking == flashagent_llm::ThinkingEffort::Auto {
            opts.thinking = flashagent_llm::ThinkingEffort::Medium;
        }
        let source = source.clone();
        self.warm_task = Some((
            key,
            tokio::spawn(async move {
                use futures::StreamExt;
                let Ok(mut stream) = source.turn_with_options(&messages, &specs, &opts).await else {
                    return false;
                };
                let mut ok = true;
                while let Some(event) = stream.next().await {
                    ok &= event.is_ok();
                }
                ok
            }),
        ));
    }

    /// The next request's prefix is already cached after a turn.
    pub(crate) fn note_prompt_cached(&mut self, source: &BackendSource, perm: &'static PermissionedTools, memory_block: &str) {
        self.cache_warm_key = Some(self.warm_key(&source.0.base_url(), perm, memory_block).0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_server_nearby_is_warmed() {
        use flashagent_llm::{ApiProtocol, Endpoint};
        let warm = |protocol, url: &str| worth_warming(&Endpoint::new(protocol, url, None), None);
        assert!(warm(ApiProtocol::OpenAi, "http://localhost:1234/v1"));
        assert!(warm(ApiProtocol::OpenAi, "http://192.168.1.20:8000/v1"), "vLLM on a LAN box");
        assert!(warm(ApiProtocol::Ollama, "http://gpu-box:11434"));
        assert!(!warm(ApiProtocol::OpenAi, "https://openrouter.ai/api/v1"), "billed, and fast anyway");
        assert!(!warm(ApiProtocol::Anthropic, "https://api.anthropic.com"));
        assert!(!warm(ApiProtocol::Gemini, "http://localhost:8080"), "a tunnel to Google is still Google");
        let tunnelled = Endpoint::new(ApiProtocol::OpenAi, "https://llm.example.com/v1", None);
        assert!(worth_warming(&tunnelled, Some(flashagent_llm::thinking::ServerKind::LmStudio)), "LM Studio behind a proxy caches all the same");
    }

    #[test]
    fn a_fresh_session_warms_the_system_prompt_voice_and_memory_block() {
        let history = vec![ChatMessage::system("SYSTEM")];
        let prelude = vec![ChatMessage::user("q"), ChatMessage::assistant("a")];
        let m = warm_messages(&history, &prelude, "MEMORY");
        let contents: Vec<&str> = m.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["SYSTEM", "q", "a", "MEMORY\n\n---\n\n"]);
    }

    #[test]
    fn the_warmed_prefix_is_what_the_first_prompt_begins_with() {
        let memory = "MEMORY";
        let first_prompt = format!("{memory}\n\n---\n\nfix main.rs");
        let warmed = warm_messages(&[ChatMessage::system("S")], &[], memory);
        assert!(first_prompt.starts_with(&warmed.last().unwrap().content));
    }

    #[test]
    fn a_resumed_session_warms_all_of_it() {
        let history = vec![ChatMessage::system("S"), ChatMessage::user("MEMORY\n\n---\n\nhello"), ChatMessage::assistant("hi there")];
        let m = warm_messages(&history, &[], "MEMORY");
        assert_eq!(m.len(), 4);
        assert_eq!(m[2].content, "hi there");
        assert_eq!(m[3].content, "hi", "the memory block goes only in front of the first prompt");
    }

    #[test]
    fn a_change_of_model_voice_or_history_is_a_new_prefix() {
        let h = vec![ChatMessage::system("S")];
        let base = prefix_key("s", "m", &warm_messages(&h, &[], ""), &[]);
        assert_eq!(base, prefix_key("s", "m", &warm_messages(&h, &[], ""), &[]));
        assert_ne!(base, prefix_key("s", "other", &warm_messages(&h, &[], ""), &[]));
        assert_ne!(base, prefix_key("another server", "m", &warm_messages(&h, &[], ""), &[]), "a provider switch warms the new server");
        assert_ne!(base, prefix_key("s", "m", &warm_messages(&h, &[ChatMessage::user("q")], ""), &[]));
        let mut longer = h.clone();
        longer.push(ChatMessage::user("x"));
        assert_ne!(base, prefix_key("s", "m", &warm_messages(&longer, &[], ""), &[]));
    }
}
