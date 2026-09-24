//! Backend failures in words a person can act on. Deliberately shallow: a few
//! cases cover nearly every failure, local or hosted, and anything unknown
//! keeps its original text.

#[derive(Debug, Clone, PartialEq)]
pub struct Explained {
    pub headline: String,
    pub hint: Option<String>,
    /// Kept for the transcript.
    pub raw: String,
}

/// `url` and `model` are named in the message.
pub fn explain(raw: &str, url: &str, model: &str) -> Explained {
    let lower = raw.to_lowercase();
    let base = url.trim_end_matches('/');
    // Advice differs: a local server is started and its log read; a hosted one is reached.
    let local = flashagent_core::url_host(base).is_some_and(|host| flashagent_core::is_local_host(&host));

    // A provider's own filters withheld the answer (`LlmError::Forbidden`); its
    // message already says why.
    if let Some(why) = raw.split_once("forbidden: ").map(|(_, why)| why.trim()).filter(|why| !why.is_empty()) {
        return Explained {
            headline: why.to_string(),
            hint: Some("Rephrase the request, or switch to another provider with /provider.".to_string()),
            raw: raw.to_string(),
        };
    }

    // The most common: the server is not started, or is on another port.
    if lower.contains("connection refused")
        || lower.contains("error sending request")
        || lower.contains("tcp connect error")
        || lower.contains("dns error")
        || lower.contains("failed to lookup")
    {
        let hint = if local {
            "Start your server (LM Studio, llama.cpp, Ollama), or switch to another provider with /provider."
        } else {
            "Check the network connection and the address, or switch to another provider with /provider."
        };
        return Explained { headline: format!("No model server answered at {base}"), hint: Some(hint.to_string()), raw: raw.to_string() };
    }

    if lower.contains("model_not_found")
        || (lower.contains("404") && lower.contains("model"))
        || lower.contains("no model loaded")
        || lower.contains("model unloaded")
    {
        return Explained {
            headline: format!("The server at {base} does not have {model} loaded"),
            hint: Some("Press F3 to pick a model it does have, or load it in your server.".to_string()),
            raw: raw.to_string(),
        };
    }

    if lower.contains("context length")
        || lower.contains("context window")
        || lower.contains("too many tokens")
        || lower.contains("maximum context")
        || lower.contains("kv cache")
    {
        return Explained {
            headline: "The conversation no longer fits in the model's context".to_string(),
            hint: Some("/compact summarizes the older turns and frees the space.".to_string()),
            raw: raw.to_string(),
        };
    }

    if lower.contains("401") || lower.contains("403") || lower.contains("unauthorized")
        || lower.contains("invalid api key") || lower.contains("incorrect api key")
        // Gemini answers a bad key with a 400.
        || lower.contains("api key not valid") || lower.contains("api_key_invalid")
        || lower.contains("invalid x-api-key") || lower.contains("authentication_error")
    {
        return Explained {
            headline: format!("{base} rejected the API key"),
            hint: Some("Set its key in /provider → Edit providers, or in the provider's environment variable.".to_string()),
            raw: raw.to_string(),
        };
    }

    // Anthropic's 529 and in-stream `overloaded_error`: busy, not broken.
    if lower.contains("overloaded") || lower.contains("returned 529") {
        return Explained {
            headline: format!("{base} is overloaded right now"),
            hint: Some("Wait a moment and press Ctrl+R, or switch provider with /provider.".to_string()),
            raw: raw.to_string(),
        };
    }

    if lower.contains("429") || lower.contains("rate limit") || lower.contains("quota") {
        return Explained {
            headline: format!("{base} is rate-limiting this key"),
            hint: Some("Wait and press Ctrl+R, or switch to a local provider with /provider.".to_string()),
            raw: raw.to_string(),
        };
    }

    if lower.contains("timed out") || lower.contains("timeout") || lower.contains("operation timed out") {
        return Explained {
            headline: format!("{base} accepted the request and then sent nothing"),
            hint: Some(
                "A large model can take a while to start; if it keeps happening, check the \
                 server's own log."
                    .to_string(),
            ),
            raw: raw.to_string(),
        };
    }

    // Usually the server ran out of memory and killed the model.
    if lower.contains("stream interrupted") || lower.contains("connection reset")
        || lower.contains("incomplete message") || lower.contains("body stream")
    {
        let hint = if local {
            "The server dropped the connection \u{2014} often it ran out of memory. Its log will say. Ctrl+R retries."
        } else {
            "The provider ended the answer with the error below. Ctrl+R retries."
        };
        return Explained { headline: "The answer stopped mid-stream".to_string(), hint: Some(hint.to_string()), raw: raw.to_string() };
    }

    if lower.contains("500") || lower.contains("502") || lower.contains("503") {
        return Explained {
            headline: format!("{base} returned a server error"),
            hint: Some("Its own log will say why; Ctrl+R retries.".to_string()),
            raw: raw.to_string(),
        };
    }

    // Unrecognised: keep the original text rather than invent a cause.
    Explained { headline: raw.trim().to_string(), hint: None, raw: raw.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "http://localhost:1234/v1";

    fn headline(raw: &str) -> String {
        explain(raw, URL, "gemma-4-e2b").headline
    }

    #[test]
    fn a_dead_server_says_so_and_says_what_to_do() {
        let e = explain(
            "llm: http: error sending request for url (http://localhost:1234/v1/chat/completions)",
            URL,
            "gemma-4-e2b",
        );
        assert_eq!(e.headline, "No model server answered at http://localhost:1234/v1");
        assert!(e.hint.unwrap().contains("Start your server"));
        assert!(e.raw.contains("chat/completions"), "the original is kept");
    }

    #[test]
    fn a_missing_model_points_at_the_model_picker() {
        let e = explain(
            r#"backend returned 404: {"error":{"message":"model_not_found"}}"#,
            URL,
            "qwen3.6-35b",
        );
        assert!(e.headline.contains("does not have qwen3.6-35b loaded"), "{}", e.headline);
        assert!(e.hint.unwrap().contains("F3"));
    }

    #[test]
    fn a_full_context_points_at_compact() {
        let e = explain(
            r#"backend returned 400: {"error":"Trying to keep the first 4096 tokens when context the overflows. However, the model is loaded with context length of only 4096 tokens, which is not enough. Try to load the model with a larger context length"#,
            URL,
            "m",
        );
        assert!(e.headline.contains("no longer fits"), "{}", e.headline);
        assert!(e.hint.unwrap().contains("/compact"));
    }

    #[test]
    fn credentials_rate_limits_and_timeouts_are_told_apart() {
        assert!(headline("backend returned 401: unauthorized").contains("rejected the API key"));
        assert!(headline("backend returned 429: rate limit exceeded").contains("rate-limiting"));
        assert!(headline("http: operation timed out").contains("sent nothing"));
        assert!(headline("stream interrupted: connection reset by peer").contains("mid-stream"));
    }

    #[test]
    fn something_unrecognised_keeps_its_own_words() {
        // Better a strange message than a confident wrong one.
        let e = explain("llm: tool use disabled by policy", URL, "m");
        assert_eq!(e.headline, "llm: tool use disabled by policy");
        assert_eq!(e.hint, None);
    }

    #[test]
    fn a_withheld_answer_says_why_in_the_provider_s_words() {
        let e = explain("llm: forbidden: Gemini refused the request (SAFETY) because its safety filters flagged it", "https://generativelanguage.googleapis.com/v1beta", "gemini-3-pro");
        assert_eq!(e.headline, "Gemini refused the request (SAFETY) because its safety filters flagged it");
        assert!(e.hint.unwrap().contains("Rephrase"));
    }

    #[test]
    fn a_hosted_provider_gets_hosted_advice() {
        let cloud = "https://api.anthropic.com";
        let dropped = explain("llm: stream interrupted: {\"type\":\"api_error\"}", cloud, "m");
        assert!(!dropped.hint.as_deref().unwrap().contains("memory"), "a hosted API did not run out of memory");
        assert!(explain("llm: stream interrupted: connection reset", URL, "m").hint.unwrap().contains("memory"));
        assert!(explain("llm: http: error sending request for url (https://api.anthropic.com/v1/messages)", cloud, "m").hint.unwrap().contains("network"));
        assert!(explain("llm: stream interrupted: Overloaded", cloud, "m").headline.contains("overloaded"));
        let gemini_key = r#"llm: backend returned 400: {"error":{"code":400,"message":"API key not valid. Please pass a valid API key.","status":"INVALID_ARGUMENT"}}"#;
        assert!(explain(gemini_key, "https://generativelanguage.googleapis.com/v1beta", "m").headline.contains("rejected the API key"));
    }
}
