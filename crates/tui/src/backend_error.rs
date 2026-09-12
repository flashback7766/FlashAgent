//! Turning backend failures into something a person can act on.
//!
//! `llm: http: error sending request for url (http://localhost:1234/v1/…)` is
//! the truth and it is useless: it does not say what broke or what to do. The
//! classification here is deliberately shallow — a handful of failures cover
//! nearly every bad evening with a local model, and anything unrecognised
//! keeps its original text rather than being smoothed into a guess.

/// A failure explained: what happened, and what the user can do next.
#[derive(Debug, Clone, PartialEq)]
pub struct Explained {
    /// One line, in plain words.
    pub headline: String,
    /// What to do about it, when there is something.
    pub hint: Option<String>,
    /// The original message, kept for the transcript.
    pub raw: String,
}

/// Explain a backend error. `url` and `model` are what the request was aimed
/// at, so the message can name them instead of describing them.
pub fn explain(raw: &str, url: &str, model: &str) -> Explained {
    let lower = raw.to_lowercase();
    let base = url.trim_end_matches('/');

    // Nothing listening. By far the most common one: the server is not
    // started, or it is on another port.
    if lower.contains("connection refused")
        || lower.contains("error sending request")
        || lower.contains("tcp connect error")
        || lower.contains("dns error")
        || lower.contains("failed to lookup")
    {
        return Explained {
            headline: format!("No model server answered at {base}"),
            hint: Some(
                "Start your server (LM Studio, llama.cpp, Ollama), or point FlashAgent \
                 somewhere else with Tab → LLM, or --url."
                    .to_string(),
            ),
            raw: raw.to_string(),
        };
    }

    // The server is there; the model is not.
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

    // Too much conversation for the model.
    if lower.contains("context length")
        || lower.contains("context window")
        || lower.contains("too many tokens")
        || lower.contains("maximum context")
        || lower.contains("kv cache")
    {
        return Explained {
            headline: "The conversation no longer fits in the model's context".to_string(),
            hint: Some("/compact summarises the older turns and frees the space.".to_string()),
            raw: raw.to_string(),
        };
    }

    // Credentials.
    if lower.contains("401") || lower.contains("403") || lower.contains("unauthorized")
        || lower.contains("invalid api key") || lower.contains("incorrect api key")
    {
        return Explained {
            headline: format!("{base} rejected the API key"),
            hint: Some("Set or fix the key in Tab → LLM, or in ~/.flashagent/config.json.".to_string()),
            raw: raw.to_string(),
        };
    }

    // Rate limited or out of credit.
    if lower.contains("429") || lower.contains("rate limit") || lower.contains("quota") {
        return Explained {
            headline: format!("{base} is rate-limiting this key"),
            hint: Some("Wait and press Ctrl+R, or switch to a local model with F3.".to_string()),
            raw: raw.to_string(),
        };
    }

    // The server took the request and went quiet.
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

    // Connection dropped mid-answer — typically the server ran out of memory
    // and killed the model.
    if lower.contains("stream interrupted") || lower.contains("connection reset")
        || lower.contains("incomplete message") || lower.contains("body stream")
    {
        return Explained {
            headline: "The answer stopped mid-stream".to_string(),
            hint: Some(
                "The server dropped the connection — often it ran out of memory. Its log will \
                 say. Ctrl+R retries."
                    .to_string(),
            ),
            raw: raw.to_string(),
        };
    }

    // A 5xx with nothing recognisable in it.
    if lower.contains("500") || lower.contains("502") || lower.contains("503") {
        return Explained {
            headline: format!("{base} returned a server error"),
            hint: Some("Its own log will say why; Ctrl+R retries.".to_string()),
            raw: raw.to_string(),
        };
    }

    // Unrecognised: say so honestly rather than inventing a cause.
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
        let e = explain("llm: forbidden: tool use disabled by policy", URL, "m");
        assert_eq!(e.headline, "llm: forbidden: tool use disabled by policy");
        assert_eq!(e.hint, None);
    }
}
