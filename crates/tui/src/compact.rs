use super::*;

/// Marker that introduces the rolling summary inside the system message.
pub(crate) const COMPACTED_MARK: &str = "\n\n[Compacted Conversation History]:\n";

pub(crate) async fn compact_context(
    source: &BackendSource,
    history: &mut Vec<ChatMessage>,
    focus_prompt: Option<&str>,
) -> Option<usize> {
    use futures::StreamExt;

    // Keep the current turn intact, starting at its user message: cutting
    // anywhere else can orphan tool results from their tool calls, or leave an
    // assistant message first — both rejected by strict servers/templates.
    let split_idx = history.iter().rposition(|m| m.role == flashagent_llm::Role::User)?;
    if split_idx <= 1 {
        return None;
    }

    let before_tokens: usize = history.iter().map(|m| m.content.len() / 4 + 4).sum();

    // A previous summary lives in the system message; fold it in so repeated
    // compaction never forgets older turns.
    let (base_system, prior_summary) = match history[0].content.find(COMPACTED_MARK) {
        Some(pos) => (history[0].content[..pos].to_string(), Some(history[0].content[pos + COMPACTED_MARK.len()..].to_string())),
        None => (history[0].content.clone(), None),
    };

    let mut to_compact = String::new();
    if let Some(prior) = &prior_summary {
        to_compact.push_str(&format!("Earlier summary:\n{prior}\n\n"));
    }
    for msg in &history[1..split_idx] {
        let role_str = match msg.role {
            flashagent_llm::Role::User => "User",
            flashagent_llm::Role::Assistant => "Assistant",
            flashagent_llm::Role::System => "System",
            flashagent_llm::Role::Tool => "Tool",
        };
        let clean_content = if msg.role == flashagent_llm::Role::User {
            extract_user_prompt(&msg.content)
        } else {
            msg.content.as_str()
        };
        to_compact.push_str(&format!("{role_str}: {}\n\n", clean_content));
    }

    let focus_text = if let Some(focus) = focus_prompt {
        format!("\nSpecial user focus/instructions: preserve details regarding: {focus}\n")
    } else {
        String::new()
    };

    // The summary replaces the conversation, so it is asked for by section:
    // a free-form précis loses exactly the things the next turn needs — what
    // the user actually asked for, which files are in play, and what was
    // about to happen next.
    let summary_prompt = format!(
        "Summarize this conversation so that work can continue from the summary alone.\n\
         Write these sections, in this order, and leave out any that has nothing in it:\n\
         1. GOAL — what the user is trying to achieve, in their own words where possible.\n\
         2. DECISIONS — choices made and the reason for each.\n\
         3. FILES — every file read or changed, with what changed in it.\n\
         4. FACTS — commands, versions, paths, numbers and errors that were established. Keep them exact.\n\
         5. STATE — what is done, what is verified, what is still broken.\n\
         6. NEXT — what was about to be done.\n\
         Keep every instruction and preference the user stated. Drop pleasantries, \
         repeated steps and tool output that led nowhere. Write in the language of the \
         conversation.\n\
         {focus_text}\n\
         Conversation:\n\n\
         {to_compact}\n\n\
         Output only the summary."
    );

    let msgs = vec![
        ChatMessage::system("You are a technical context compaction engine."),
        ChatMessage::user(summary_prompt),
    ];
    let opts = flashagent_llm::TurnOptions {
        thinking: flashagent_llm::ThinkingEffort::Off,
        temperature: Some(0.2),
        ..Default::default()
    };

    // The old budget was twelve seconds for the whole summary and five
    // between chunks. A 35B model on a laptop writes at ten tokens a second,
    // so every summary it was asked for timed out and the conversation was
    // replaced by a list of truncated snippets instead. Prefill of a long
    // history alone can take a minute.
    let summary = if let Ok(Ok(mut stream)) = tokio::time::timeout(
        std::time::Duration::from_secs(300),
        source.turn_with_options(&msgs, &[], &opts),
    ).await {
        let mut text = String::new();
        while let Ok(Some(Ok(ev))) = tokio::time::timeout(std::time::Duration::from_secs(120), stream.next()).await {
            if let flashagent_llm::LlmEvent::TextDelta(d) = ev {
                text.push_str(&d);
            }
        }
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            trimmed.to_string()
        } else {
            fallback_summary(&history[1..split_idx], prior_summary.as_deref())
        }
    } else {
        fallback_summary(&history[1..split_idx], prior_summary.as_deref())
    };

    let mut new_history = Vec::with_capacity(history.len() - split_idx + 1);
    new_history.push(ChatMessage::system(format!("{base_system}{COMPACTED_MARK}{summary}")));
    new_history.extend_from_slice(&history[split_idx..]);

    let after_tokens: usize = new_history.iter().map(|m| m.content.len() / 4 + 4).sum();
    *history = new_history;

    let freed = before_tokens.saturating_sub(after_tokens);
    Some(freed.max(1))
}

pub(crate) fn fallback_summary(messages: &[ChatMessage], prior: Option<&str>) -> String {
    let mut out = String::from("Previous conversation summary (auto-compacted):\n");
    if let Some(prior) = prior {
        out.push_str(prior.trim_end());
        out.push('\n');
    }
    for (i, msg) in messages.iter().enumerate() {
        let role = match msg.role {
            flashagent_llm::Role::User => "User",
            flashagent_llm::Role::Assistant => "Assistant",
            flashagent_llm::Role::System => "System",
            flashagent_llm::Role::Tool => "Tool",
        };
        let clean = if msg.role == flashagent_llm::Role::User {
            extract_user_prompt(&msg.content)
        } else {
            msg.content.as_str()
        };
        let snippet = flashagent_tui::truncate_middle(clean, 120);
        out.push_str(&format!("{}. {role}: {snippet}\n", i + 1));
    }
    out
}
