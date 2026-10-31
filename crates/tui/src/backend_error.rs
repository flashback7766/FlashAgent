//! Backend failures in words a person can act on. Two layers: first the
//! server's own sentence is dug out of whatever shape it arrived in — every
//! provider nests it differently, and a gateway wraps it again — then the
//! sentence and the status code are matched against the few situations that
//! cover nearly every failure, local or hosted. Anything unknown keeps the
//! provider's own words; only a body with nothing readable in it keeps the
//! raw dump.

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
    let said = said_message(raw);
    // The code the server named, when it did: "backend returned 402: …".
    let code = raw
        .split_once("returned ")
        .and_then(|(_, rest)| rest.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|digits| digits.parse::<u16>().ok());

    // A provider's own filters withheld the answer (`LlmError::Forbidden`); its
    // message already says why.
    if let Some(why) = raw.split_once("forbidden: ").map(|(_, why)| why.trim()).filter(|why| !why.is_empty()) {
        return Explained {
            headline: why.to_string(),
            hint: Some(crate::i18n::tr("Rephrase the request, or switch to another provider with /provider.").to_string()),
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
            crate::i18n::tr("Start your server (LM Studio, llama.cpp, Ollama), or switch to another provider with /provider.")
        } else {
            crate::i18n::tr("Check the network connection and the address, or switch to another provider with /provider.")
        };
        return Explained { headline: crate::i18n::tr_fill("No model server answered at {base}", &[("base", base)]), hint: Some(hint.to_string()), raw: raw.to_string() };
    }

    if lower.contains("model_not_found")
        || lower.contains("no endpoints found")
        || lower.contains("does not exist")
        || (lower.contains("404") && lower.contains("model"))
        || lower.contains("no model loaded")
        || lower.contains("model unloaded")
    {
        return Explained {
            headline: crate::i18n::tr_fill("The server at {base} does not have {model} loaded", &[("base", base), ("model", model)]),
            hint: Some(crate::i18n::tr("Press F3 to pick a model it does have, or load it in your server.").to_string()),
            raw: raw.to_string(),
        };
    }

    if lower.contains("context length")
        || lower.contains("context window")
        || lower.contains("context_length_exceeded")
        || lower.contains("prompt is too long")
        || lower.contains("input tokens exceed")
        || lower.contains("too many tokens")
        || lower.contains("maximum context")
        || lower.contains("kv cache")
    {
        return Explained {
            headline: crate::i18n::tr("The conversation no longer fits in the model's context").to_string(),
            hint: Some(crate::i18n::tr("/compact summarizes the older turns and frees the space.").to_string()),
            raw: raw.to_string(),
        };
    }

    // 402: the account is out of credit. The provider's message says how much
    // is left; its JSON (with the account id) stays out of the headline.
    if code == Some(402) || lower.contains("returned 402") || lower.contains("insufficient credit") || lower.contains("insufficient_quota") {
        let afford = said.as_deref().and_then(|m| m.split_once("To increase").map(|(before, _)| before.trim().to_string()));
        return Explained {
            headline: crate::i18n::tr_fill("{base} says the account has too little credit", &[("base", base)]),
            hint: Some(afford.map_or_else(
                || crate::i18n::tr("Add credit at the provider, or switch to another provider or a free model with /provider and F3.").to_string(),
                |m| crate::i18n::tr_fill("{m}. Add credit, or pick a free model with F3.", &[("m", &m)]),
            )),
            // The JSON carries the account id and adds nothing to the two lines.
            raw: String::new(),
        };
    }

    if matches!(code, Some(401) | Some(403))
        || lower.contains("401") || lower.contains("403") || lower.contains("unauthorized")
        || lower.contains("invalid api key") || lower.contains("invalid_api_key") || lower.contains("incorrect api key")
        // A key the server never saw at all: OpenCode's gateway words it its own way.
        || lower.contains("missing api key") || lower.contains("requires a credential")
        // Gemini answers a bad key with a 400.
        || lower.contains("api key not valid") || lower.contains("api_key_invalid")
        || lower.contains("invalid x-api-key") || lower.contains("authentication_error")
    {
        return Explained {
            headline: crate::i18n::tr_fill("{base} rejected the API key", &[("base", base)]),
            hint: Some(crate::i18n::tr("Set its key in /provider → Edit providers, or in the provider's environment variable.").to_string()),
            raw: raw.to_string(),
        };
    }

    // A model gated by region or workspace policy: the provider's message is
    // the instruction (which switch to flip), and there is nothing to add.
    if lower.contains("unsupported_region")
        || lower.contains("unsupported_country")
        || lower.contains("privacy settings")
        || (lower.contains("region") && said.as_deref().is_some_and(|s| s.len() > 20))
    {
        return Explained {
            headline: said.unwrap_or_else(|| crate::i18n::tr("The provider does not serve this model here").to_string()),
            hint: Some(crate::i18n::tr("The provider's message says what to change; Ctrl+R retries once it is done.").to_string()),
            raw: raw.to_string(),
        };
    }

    // Anthropic's 529 and in-stream `overloaded_error`: busy, not broken.
    if lower.contains("overloaded") || code == Some(529) || lower.contains("returned 529") {
        return Explained {
            headline: crate::i18n::tr_fill("{base} is overloaded right now", &[("base", base)]),
            hint: Some(crate::i18n::tr("Wait a moment and press Ctrl+R, or switch provider with /provider.").to_string()),
            raw: raw.to_string(),
        };
    }

    if code == Some(429) || lower.contains("429") || lower.contains("rate limit") || lower.contains("quota") {
        return Explained {
            headline: crate::i18n::tr_fill("{base} is rate-limiting this key", &[("base", base)]),
            hint: Some(crate::i18n::tr("Wait and press Ctrl+R, or switch to a local provider with /provider.").to_string()),
            raw: raw.to_string(),
        };
    }

    if lower.contains("timed out") || lower.contains("timeout") || lower.contains("operation timed out") {
        return Explained {
            headline: crate::i18n::tr_fill("{base} accepted the request and then sent nothing", &[("base", base)]),
            hint: Some(
                crate::i18n::tr("A large model can take a while to start; if it keeps happening, check the server's own log.")
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
            crate::i18n::tr("The server dropped the connection \u{2014} often it ran out of memory. Its log will say. Ctrl+R retries.")
        } else {
            crate::i18n::tr("The provider ended the answer with the error below. Ctrl+R retries.")
        };
        return Explained {
            headline: crate::i18n::tr("The answer stopped mid-stream").to_string(),
            hint: Some(hint.to_string()),
            raw: raw.to_string(),
        };
    }

    if matches!(code, Some(500) | Some(502) | Some(503) | Some(504))
        || lower.contains("returned 500") || lower.contains("returned 502") || lower.contains("returned 503")
    {
        return Explained {
            headline: format!("{base} returned a server error"),
            hint: Some(crate::i18n::tr("Its own log will say why; Ctrl+R retries.").to_string()),
            raw: raw.to_string(),
        };
    }

    // Unrecognised, but readable: the provider's own words, without the JSON
    // around them. Better a strange message than a confident wrong one — and
    // better still without the dump where the words belong.
    if let Some(said) = said {
        return Explained { headline: said, hint: None, raw: raw.to_string() };
    }
    Explained { headline: raw.trim().to_string(), hint: None, raw: raw.to_string() }
}

/// The first JSON value in `raw`, read past whatever prefix carries it.
fn json_in(raw: &str) -> Option<serde_json::Value> {
    let open = raw.find(['{', '['])?;
    serde_json::Deserializer::from_str(&raw[open..]).into_iter::<serde_json::Value>().next()?.ok()
}

/// The readable sentence in a server's error: the first message-like string,
/// depth-first. Providers nest it one level deep, gateways add another, and
/// nearly every one calls the field something slightly different.
fn message_in(value: &serde_json::Value) -> Option<String> {
    const KEYS: [&str; 5] = ["message", "msg", "detail", "error_description", "description"];
    if let Some(said) = KEYS.iter().find_map(|key| value.get(*key).and_then(|v| v.as_str())) {
        let said = said.trim();
        if !said.is_empty() {
            return Some(said.to_string());
        }
    }
    match value {
        serde_json::Value::Object(map) => {
            // "error" is a string as often as it is an object, and it reads either way.
            match map.get("error") {
                Some(serde_json::Value::String(said)) if !said.trim().is_empty() => Some(said.trim().to_string()),
                Some(child) if child.is_object() || child.is_array() => {
                    message_in(child).or_else(|| map.values().filter(|v| v.is_object() || v.is_array()).find_map(message_in))
                }
                _ => map.values().filter(|v| v.is_object() || v.is_array()).find_map(message_in),
            }
        }
        serde_json::Value::Array(items) => items.iter().find_map(message_in),
        _ => None,
    }
}

/// A gateway's own words around the provider's: "Upstream request failed: …"
/// says nothing the sentence after it does not say better.
fn unwrap_prefixes(mut said: String) -> String {
    loop {
        let trimmed = said.trim();
        let stripped = ["Upstream request failed:", "Error:", "error:"]
            .iter()
            .find_map(|prefix| trimmed.strip_prefix(prefix).map(str::trim_start).filter(|rest| !rest.is_empty()));
        match stripped {
            Some(rest) => said = rest.to_string(),
            None => return trimmed.to_string(),
        }
    }
}

/// The provider's own sentence, dug out of any shape; `None` when the body
/// carries nothing readable.
fn said_message(raw: &str) -> Option<String> {
    message_in(&json_in(raw)?).map(unwrap_prefixes)
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
    fn an_account_out_of_credit_says_so_without_the_json() {
        let e = explain(
            r#"llm: backend returned 402: {"error":{"message":"This request requires more credits. You requested up to 131072 tokens, but can only afford 68094. To increase, visit https://openrouter.ai/settings/credits","code":402},"user_id":"user_abc"}"#,
            "https://openrouter.ai/api/v1",
            "m",
        );
        assert!(e.headline.contains("too little credit"), "{}", e.headline);
        let hint = e.hint.unwrap();
        assert!(hint.contains("can only afford 68094") && !hint.contains("settings/credits"), "{hint}");
        assert!(e.raw.is_empty(), "the account id stays out of the transcript: {}", e.raw);
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

    #[test]
    fn a_gateways_wrapper_is_unwrapped_and_the_json_goes_back_to_a_footnote() {
        // Seen live on OpenCode Go: a 400 whose message a gateway wrapped, and
        // which matched nothing — so the whole dump became the headline.
        let e = explain(
            r#"llm: backend returned 400: {"error":{"type":"server_error","message":"Upstream request failed: This Go model requires Global regions. Select Global in your workspace's Privacy settings to use it."}}"#,
            "https://opencode.ai/inference/go/v1",
            "deepseek-v4.1-flash",
        );
        assert_eq!(
            e.headline,
            "This Go model requires Global regions. Select Global in your workspace's Privacy settings to use it."
        );
        assert!(e.hint.is_some_and(|h| h.contains("Ctrl+R")));
        assert!(e.raw.contains("Upstream request failed"), "the original stays for the transcript");
    }

    #[test]
    fn a_message_is_found_in_any_shape_it_is_nested_in() {
        let find = |body: &str| explain(&format!("llm: backend returned 400: {body}"), URL, "m").headline;
        assert_eq!(find(r#"{"a":{"b":{"message":"the disk is full"}}}"#), "the disk is full", "two levels deep");
        assert_eq!(find(r#"{"errors":[{"message":"first of them"}]}"#), "first of them", "an array of errors");
        assert_eq!(
            find(r#"{"detail":[{"loc":["body","model"],"msg":"field required","type":"missing"}]}"#),
            "field required",
            "FastAPI's validation shape"
        );
        assert_eq!(find(r#"{"error":"bare string"}"#), "bare string", "llama.cpp's bare error string");
        // An unknown code but a readable body shows the sentence, not the
        // dump; the dim raw line under it keeps the rest.
        let e = explain(r#"llm: backend returned 418: {"detail":"short and stout"}"#, URL, "m");
        assert_eq!(e.headline, "short and stout");
        assert_eq!(e.hint, None, "nothing was recognised, so nothing is promised");
        assert!(e.raw.contains("418"));
    }

    #[test]
    fn a_region_gate_quotes_the_provider_s_instruction() {
        let e = explain(
            r#"llm: backend returned 400: {"error":{"message":"This model is not available in your region."}}"#,
            "https://example.api/v1",
            "m",
        );
        assert_eq!(e.headline, "This model is not available in your region.");
        assert!(e.hint.unwrap().contains("Ctrl+R"));
    }

    #[test]
    fn a_key_the_server_never_saw_is_still_an_answer_about_keys() {
        // OpenCode's gateway answers a missing key with its own error type.
        let e = explain(
            r#"llm: backend returned 401: {"type":"error","error":{"type":"AuthError","message":"Missing API key."}}"#,
            "https://opencode.ai/inference/go/v1",
            "m",
        );
        assert!(e.headline.contains("rejected the API key"), "{}", e.headline);
        assert!(e.hint.unwrap().contains("/provider"));
    }
}
