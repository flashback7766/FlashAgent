//! Which wire protocol a server speaks, and where its requests go.

use serde::{Deserialize, Serialize};

/// Chosen with the provider, never guessed per request: the same address can
/// speak two protocols (Ollama answers both `/api/chat` and `/v1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiProtocol {
    /// `/chat/completions`: LM Studio, llama.cpp, vLLM, OpenRouter, OpenAI,
    /// DeepSeek, Mistral, Groq and anything else that copies it.
    #[default]
    #[serde(alias = "openai-compatible", alias = "openai_compatible", alias = "open_ai")]
    OpenAi,
    /// Anthropic's `/v1/messages`.
    Anthropic,
    /// Google's `generateContent`.
    #[serde(alias = "google")]
    Gemini,
    /// Ollama's own `/api/chat`.
    Ollama,
}

impl ApiProtocol {
    pub const ALL: [ApiProtocol; 4] = [ApiProtocol::OpenAi, ApiProtocol::Anthropic, ApiProtocol::Gemini, ApiProtocol::Ollama];

    /// Short and stable: stored in the config and shown in the footer.
    pub fn id(self) -> &'static str {
        match self {
            ApiProtocol::OpenAi => "openai",
            ApiProtocol::Anthropic => "anthropic",
            ApiProtocol::Gemini => "gemini",
            ApiProtocol::Ollama => "ollama",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ApiProtocol::OpenAi => "OpenAI-compatible",
            ApiProtocol::Anthropic => "Anthropic",
            ApiProtocol::Gemini => "Google Gemini",
            ApiProtocol::Ollama => "Ollama",
        }
    }

    /// For an address typed without saying what speaks there. Only hosts
    /// that speak one protocol and nothing else are recognised; anything
    /// else is taken as OpenAI-compatible, which is what most servers copy.
    pub fn detect(url: &str) -> Self {
        let url = url.trim().trim_end_matches('/').to_ascii_lowercase();
        let host = url.split("://").nth(1).unwrap_or(&url).split('/').next().unwrap_or_default();
        let path = url.split("://").nth(1).unwrap_or(&url).split_once('/').map(|(_, p)| p).unwrap_or_default();
        if host == "api.anthropic.com" {
            ApiProtocol::Anthropic
        } else if host == "generativelanguage.googleapis.com" && !path.ends_with("openai") {
            ApiProtocol::Gemini
        } else if host.ends_with(":11434") && !path.split('/').any(|seg| seg == "v1") {
            ApiProtocol::Ollama
        } else {
            ApiProtocol::OpenAi
        }
    }
}

impl std::fmt::Display for ApiProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// A server and how to talk to it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Endpoint {
    pub protocol: ApiProtocol,
    /// Without a trailing slash.
    pub url: String,
    /// `None` for a server that needs none; a blank key is `None`.
    pub api_key: Option<String>,
    /// The context window to run models with, where the server lets the
    /// client choose (Ollama's `num_ctx`). `None` leaves it to the client.
    pub context_window: Option<usize>,
}

impl Endpoint {
    pub fn new(protocol: ApiProtocol, url: impl Into<String>, api_key: Option<String>) -> Self {
        let url: String = url.into();
        Self {
            protocol,
            url: url.trim().trim_end_matches('/').to_string(),
            api_key: api_key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty()),
            context_window: None,
        }
    }

    pub fn with_context_window(mut self, tokens: Option<usize>) -> Self {
        self.context_window = tokens.filter(|&n| n > 0);
        self
    }

    /// The protocol read from the address; see [`ApiProtocol::detect`].
    pub fn detect(url: impl Into<String>, api_key: Option<String>) -> Self {
        let url: String = url.into();
        Self::new(ApiProtocol::detect(&url), url, api_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_hosts_that_speak_one_protocol_are_recognised() {
        assert_eq!(ApiProtocol::detect("https://api.anthropic.com/v1"), ApiProtocol::Anthropic);
        assert_eq!(ApiProtocol::detect("https://generativelanguage.googleapis.com/v1beta"), ApiProtocol::Gemini);
        assert_eq!(
            ApiProtocol::detect("https://generativelanguage.googleapis.com/v1beta/openai/"),
            ApiProtocol::OpenAi,
            "Google's OpenAI-compatible gateway"
        );
        assert_eq!(ApiProtocol::detect("http://localhost:11434"), ApiProtocol::Ollama);
        assert_eq!(ApiProtocol::detect("http://localhost:11434/v1"), ApiProtocol::OpenAi, "Ollama's /v1 copies OpenAI");
        assert_eq!(ApiProtocol::detect("http://localhost:1234/v1"), ApiProtocol::OpenAi);
        assert_eq!(ApiProtocol::detect("https://openrouter.ai/api/v1"), ApiProtocol::OpenAi);
        assert_eq!(ApiProtocol::detect(""), ApiProtocol::OpenAi);
    }

    #[test]
    fn the_stored_name_reads_back_in_any_common_spelling() {
        for (text, want) in [
            ("\"openai\"", ApiProtocol::OpenAi),
            ("\"openai-compatible\"", ApiProtocol::OpenAi),
            ("\"anthropic\"", ApiProtocol::Anthropic),
            ("\"gemini\"", ApiProtocol::Gemini),
            ("\"google\"", ApiProtocol::Gemini),
            ("\"ollama\"", ApiProtocol::Ollama),
        ] {
            assert_eq!(serde_json::from_str::<ApiProtocol>(text).unwrap(), want, "{text}");
        }
        assert_eq!(serde_json::to_string(&ApiProtocol::OpenAi).unwrap(), "\"openai\"");
        for p in ApiProtocol::ALL {
            assert_eq!(serde_json::to_string(&p).unwrap(), format!("\"{}\"", p.id()));
        }
    }

    #[test]
    fn an_endpoint_has_no_trailing_slash_and_no_blank_key() {
        let e = Endpoint::new(ApiProtocol::OpenAi, " http://x/v1/ ", Some("  ".into()));
        assert_eq!(e.url, "http://x/v1");
        assert_eq!(e.api_key, None);
        assert_eq!(Endpoint::new(ApiProtocol::OpenAi, "http://x", Some(" k ".into())).api_key.as_deref(), Some("k"));
    }
}
