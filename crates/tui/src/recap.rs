use super::*;

/// The language to write our own labels in, from what the user just typed.
///
/// `None` when the message is too short to tell — a bare "ok" is not evidence
/// of anything, and switching on it would make the labels flicker.
pub(crate) fn conversation_language(text: &str) -> Option<&'static str> {
    // A slash command is our vocabulary, not theirs.
    if text.trim_start().starts_with('/') {
        return None;
    }
    let mut cyrillic = 0usize;
    let mut latin = 0usize;
    for c in text.chars().filter(|c| c.is_alphabetic()) {
        match c as u32 {
            0x0400..=0x04FF => cyrillic += 1,
            0x0041..=0x005A | 0x0061..=0x007A => latin += 1,
            _ => {}
        }
    }
    let total = cyrillic + latin;
    if total < 2 {
        return None;
    }
    // Code and identifiers are Latin even in a Russian conversation, so a
    // minority of Cyrillic still means Russian.
    if cyrillic * 4 >= total {
        Some("ru")
    } else if latin == total {
        Some("en")
    } else {
        None
    }
}

/// The language a turn is written in, by script, when it is clearly not
/// English. Script is a crude signal but a reliable one for the case that
/// matters: a model answering a Russian conversation in English.
pub(crate) fn script_language(text: &str) -> Option<&'static str> {
    let mut cyrillic = 0usize;
    let mut latin = 0usize;
    let mut cjk = 0usize;
    let mut other = 0usize;
    for c in text.chars().filter(|c| c.is_alphabetic()) {
        match c as u32 {
            0x0400..=0x04FF => cyrillic += 1,
            0x0041..=0x005A | 0x0061..=0x007A => latin += 1,
            0x3040..=0x30FF | 0x4E00..=0x9FFF => cjk += 1,
            _ => other += 1,
        }
    }
    let total = cyrillic + latin + cjk + other;
    if total < 12 {
        return None;
    }
    let share = |n: usize| n as f32 / total as f32;
    // Code and identifiers are Latin even in a Russian conversation, so a
    // minority of Cyrillic is still a Russian conversation.
    if share(cyrillic) > 0.25 {
        Some("Russian")
    } else if share(cjk) > 0.25 {
        Some("Chinese or Japanese, matching the user")
    } else {
        None
    }
}

pub(crate) async fn generate_llm_recap_and_suggestion(
    source: &BackendSource,
    history: &[ChatMessage],
) -> Option<(String, Option<String>)> {
    let last_user_idx = history.iter().rposition(|m| m.role == flashagent_llm::Role::User)?;
    let user_msg = history.get(last_user_idx)?;
    let user_prompt = extract_user_prompt(&user_msg.content);

    let turn_messages = &history[last_user_idx..];
    let assistant_msg = turn_messages.iter().rev().find(|m| m.role == flashagent_llm::Role::Assistant);
    let asst_text = assistant_msg.map(|m| m.content.as_str()).unwrap_or("");
    if asst_text.trim().is_empty() {
        return None;
    }

    let user_snip = flashagent_tui::truncate_middle(user_prompt, 200);
    let asst_snip = flashagent_tui::truncate_middle(asst_text, 600);

    let system_prompt =
        "You are a conversation analyzer. Return strictly JSON with the following fields:\n\
        {\n  \
          \"recap\": \"one concise past-tense sentence of what was explained or done\",\n  \
          \"suggestion\": \"the next message the USER will send to the assistant, in imperative mood, about the work just done (e.g. 'Show Swift code examples', 'Explain how safety works', 'Run the tests and fix what fails'). It must be something the assistant can answer on its own. NEVER ask the user for information about themselves, their project or what they need: 'Tell me about your project', 'Tell me what help you need' are WRONG - those are the assistant talking to the user. Write it in the language of the conversation. Do not repeat the existing query!\"\n\
        }\n\
        Do NOT generate any internal thinking or explanations. Start immediately with { and return only valid JSON.";

    // "the language of the conversation" is not an instruction a small model
    // reliably follows; naming the language is.
    let language_rule = match script_language(&format!("{user_snip} {asst_snip}")) {
        Some(lang) => format!(
            "\nThe conversation is in {lang}. Write BOTH fields in {lang}, not in English."
        ),
        None => String::new(),
    };
    let user_turn_info =
        format!("User prompt: {user_snip}\nAssistant response: {asst_snip}{language_rule}");
    let query_messages = vec![
        ChatMessage::system(system_prompt),
        ChatMessage::user(user_turn_info),
    ];

    let turn_opts = flashagent_llm::TurnOptions {
        thinking: flashagent_llm::ThinkingEffort::Off,
        custom_effort: Some("none".to_string()),
        temperature: Some(0.2),
        // Models asked not to think often think anyway: one observed run
        // spent 361 of its 512 tokens reasoning and was cut off before it
        // closed the JSON, so no recap ever appeared. The budget has to
        // cover the thinking the model does regardless.
        max_tokens: Some(1500),
        ..Default::default()
    };

    let overall_task = async {
        let mut stream = source.turn_with_options(&query_messages, &[], &turn_opts).await.ok()?;
        use futures::StreamExt;
        let mut collected = String::new();
        let mut reasoning_collected = String::new();
        while let Some(ev_res) = stream.next().await {
            if let Ok(ev) = ev_res {
                match ev {
                    flashagent_llm::LlmEvent::TextDelta(delta) => {
                        collected.push_str(&delta);
                    }
                    flashagent_llm::LlmEvent::ReasoningDelta(delta) => {
                        reasoning_collected.push_str(&delta);
                    }
                    flashagent_llm::LlmEvent::Done(_) => break,
                    _ => {}
                }
            }
        }
        if let Some(parsed) = parse_recap_and_suggestion_json(&collected) {
            Some(parsed)
        } else {
            parse_recap_and_suggestion_json(&reasoning_collected)
        }
    };

    // 90s timeout accommodates larger local models (e.g. 26B Gemma TTFT ~8s + generation ~5s, slow CPU inference, or heavy load)
    tokio::time::timeout(std::time::Duration::from_secs(90), overall_task).await.ok()?
}

pub(crate) fn parse_recap_and_suggestion_json(raw: &str) -> Option<(String, Option<String>)> {
    let mut storage;
    let mut text = raw.trim();

    // Strip <thought>...</thought> or <think>...</think> if model emitted reasoning in text stream
    if let Some(start_think) = text.find("<thought>") {
        if let Some(end_think) = text.find("</thought>") {
            let after = &text[end_think + "</thought>".len()..];
            let before = &text[..start_think];
            storage = format!("{before} {after}");
            text = storage.trim();
        }
    }
    if let Some(start_think) = text.find("<think>") {
        if let Some(end_think) = text.find("</think>") {
            let after = &text[end_think + "</think>".len()..];
            let before = &text[..start_think];
            storage = format!("{before} {after}");
            text = storage.trim();
        }
    }

    // Extract inside ```json ... ``` or ``` ... ``` if present
    let candidate = if let Some(code_start) = text.find("```") {
        let after_fence = &text[code_start + 3..];
        let content_start = if after_fence.to_lowercase().starts_with("json") {
            &after_fence[4..]
        } else {
            after_fence
        };
        if let Some(code_end) = content_start.find("```") {
            &content_start[..code_end]
        } else {
            content_start
        }
    } else {
        text
    };

    // Extract between outermost '{' and '}'
    let json_str = if let (Some(first_brace), Some(last_brace)) = (candidate.find('{'), candidate.rfind('}')) {
        if first_brace <= last_brace {
            &candidate[first_brace..=last_brace]
        } else {
            candidate.trim()
        }
    } else {
        candidate.trim()
    };

    // A run that hit its token limit leaves the object unclosed. Repairing
    // such JSON invents the closing quote, and with it the end of a sentence
    // the model never wrote — so when the text does not parse as it stands,
    // each field is read literally instead, and a value that never closed is
    // dropped rather than completed for it.
    let parsed: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => flashagent_llm::repair_json(json_str)
            .and_then(|r| serde_json::from_str(&r).ok())
            .filter(|_: &serde_json::Value| !json_str.trim_end().ends_with(|c: char| c != '}'))
            .unwrap_or(serde_json::Value::Null),
    };

    // The recap is the first field asked for, so it is usually complete even
    // in a cut-off answer: a finished sentence is worth more than a
    // well-formed brace.
    let recap_raw = match parsed.get("recap").and_then(|s| s.as_str()) {
        Some(found) => found.to_string(),
        None => salvage_json_string_field(json_str, "recap")?,
    };
    let recap = recap_raw.trim().trim_matches('\"').to_string();
    if recap.is_empty() || is_generic_recap(&recap) {
        return None;
    }

    let suggestion = parsed
        .get("suggestion")
        .and_then(|s| s.as_str())
        .map(str::to_string)
        .or_else(|| salvage_json_string_field(json_str, "suggestion"))
        .and_then(|s| sanitize_user_suggestion(&s));

    Some((recap, suggestion))
}

/// Read one string field out of JSON that may have been cut off mid-object.
///
/// Deliberately literal: it takes the text between the quotes after
/// `"field":` and stops at the first unescaped quote, or at the end of what
/// arrived. A value that never closed is dropped rather than guessed at.
pub(crate) fn salvage_json_string_field(raw: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\"");
    let after_key = &raw[raw.find(&key)? + key.len()..];
    let after_colon = after_key.trim_start().strip_prefix(':')?.trim_start();
    let body = after_colon.strip_prefix('"')?;

    let mut value = String::new();
    let mut chars = body.chars();
    let mut closed = false;
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('n') => value.push('\n'),
                Some('t') => value.push('\t'),
                Some(esc) => value.push(esc),
                None => break,
            },
            '"' => {
                closed = true;
                break;
            }
            other => value.push(other),
        }
    }
    let value = value.trim().to_string();
    // An unclosed value is a sentence chopped mid-word; only keep it if it
    // got far enough to say something and ended on a whole word.
    if !closed && (value.len() < 20 || !value.ends_with(['.', '!', '?'])) {
        return None;
    }
    (!value.is_empty()).then_some(value)
}

pub(crate) fn sanitize_user_suggestion(s: &str) -> Option<String> {
    let mut trimmed = s.trim().trim_matches('"').trim_matches('\'').trim();
    if trimmed.is_empty() || is_generic_suggestion(trimmed) || is_addressed_to_the_user(trimmed) {
        return None;
    }

    // Strip Russian and ASCII quotes
    trimmed = trimmed
        .trim_matches('«')
        .trim_matches('»')
        .trim_matches('"')
        .trim_matches('\'')
        .trim();

    let mut text = trimmed.to_string();
    let lower = text.to_lowercase();

    // Convert assistant question or infinitive formulations into direct user imperatives
    if lower.starts_with("would you like to see examples of ") {
        text = format!("Show code examples of {}", &trimmed["would you like to see examples of ".len()..]);
    } else if lower.starts_with("would you like to see ") {
        text = format!("Show {}", &trimmed["would you like to see ".len()..]);
    } else if lower.starts_with("would you like to know more about ") {
        text = format!("Explain more about {}", &trimmed["would you like to know more about ".len()..]);
    } else if lower.starts_with("would you like to know ") {
        text = format!("Explain {}", &trimmed["would you like to know ".len()..]);
    } else if lower.starts_with("do you want to know more about ") {
        text = format!("Explain more about {}", &trimmed["do you want to know more about ".len()..]);
    } else if lower.starts_with("do you want to know ") {
        text = format!("Explain {}", &trimmed["do you want to know ".len()..]);
    } else if lower.starts_with("tell me about ") {
        text = format!("Tell me about {}", &trimmed["tell me about ".len()..]);
    } else if lower.starts_with("show me ") {
        text = format!("Show me {}", &trimmed["show me ".len()..]);
    } else if lower.starts_with("show code examples of ") {
        text = format!("Show code examples of {}", &trimmed["show code examples of ".len()..]);
    } else if lower.starts_with("show examples ") {
        text = format!("Show examples {}", &trimmed["show examples ".len()..]);
    } else if lower.starts_with("explain to me ") {
        text = format!("Explain to me {}", &trimmed["explain to me ".len()..]);
    } else if lower.starts_with("explain ") {
        text = format!("Explain {}", &trimmed["explain ".len()..]);
    } else if lower.starts_with("give examples of ") {
        text = format!("Give examples of {}", &trimmed["give examples of ".len()..]);
    } else if lower.starts_with("learn more about ") {
        text = format!("Explain more about {}", &trimmed["learn more about ".len()..]);
    } else if is_assistant_phrased(&text) {
        return None;
    }

    // Capitalize first character
    let mut chars = text.chars();
    let first = chars.next()?;
    let capitalized: String = first.to_uppercase().chain(chars).collect();

    // Clean trailing punctuation (?, ., !) so it is an imperative prompt ready to send
    let clean = capitalized
        .trim_end_matches(['?', '.', '!', ' ', '"', '\'', '»', '«'])
        .to_string();

    if clean.is_empty() || is_generic_suggestion(&clean) {
        None
    } else {
        Some(clean)
    }
}

/// A suggestion is a message the user is about to send TO the model. Models
/// often return the opposite: the assistant asking the *user* for information
/// ("Расскажи о своём проекте", "Tell me about your setup"). Pressing → on one
/// of those sends the user their own question back, so they are dropped.
///
/// The giveaway is the object, not the verb: "расскажи о архитектуре" is a
/// fine prompt, "расскажи о своём проекте" is the assistant talking to you.
pub(crate) fn is_addressed_to_the_user(text: &str) -> bool {
    let lower = text.to_lowercase();
    // Russian: reflexive/second-person possessives always point at whoever is
    // being addressed, and here that is the user.
    const RU_POSSESSIVE: &[&str] = &[
        "свой", "своё", "свое", "своего", "своей", "своем", "своём", "своих", "свою", "своими",
        "твой", "твоё", "твое", "твоей", "твоем", "твоём", "твои", "твоих",
        "ваш ", "ваша", "ваше", "вашей", "вашем", "ваши", "вашу",
    ];
    // Phrases that only make sense coming from the assistant.
    const HANDOFF: &[&str] = &[
        "нужна помощь", "нужна ли помощь", "чем помочь", "чем могу помочь", "чем я могу помочь",
        "что тебя интересует", "что вас интересует", "дай знать", "дайте знать", "если хочешь",
        "если хотите", "уточни, что", "уточните, что",
        "your project", "your code", "your task", "your goal", "your setup", "your repo",
        "your codebase", "your use case", "your requirements", "what help", "how can i help",
        "let me know", "feel free to", "if you'd like", "if you would like",
    ];
    RU_POSSESSIVE.iter().chain(HANDOFF).any(|needle| lower.contains(needle))
}

pub(crate) fn is_assistant_phrased(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.starts_with("would you ")
        || lower.starts_with("do you want ")
        || lower.starts_with("should i ")
        || lower.starts_with("what examples ")
        || lower.starts_with("which ")
        || lower.starts_with("what would ")
        || lower.ends_with("show?")
}

pub(crate) fn is_generic_recap(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("response generated")
        || lower.contains("answer provided")
        || lower.contains("what's next")
}

pub(crate) fn is_generic_suggestion(text: &str) -> bool {
    let lower = text.to_lowercase();
    let trimmed = lower.trim_matches(|c: char| c.is_whitespace() || c == '?' || c == '.' || c == '!' || c == '"' || c == '\'');
    trimmed == "what's next"
        || trimmed == "what next"
        || trimmed == "continue"
        || trimmed == "next"
}
