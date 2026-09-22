//! `/export md`: what was said, not what the model was sent (memory block,
//! goal directive, raw tool results). Tool work is folded under `<details>`.

use super::*;

/// The session file keeps them whole.
const RESULT_CHARS: usize = 2000;

pub(crate) fn conversation_markdown(history: &[ChatMessage], session_id: &str, model: &str, cwd: &str) -> String {
    let title = history
        .iter()
        .find(|m| m.role == flashagent_llm::Role::User)
        .map(|m| extract_user_prompt(&m.content).lines().next().unwrap_or_default().trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "FlashAgent session".to_string());
    let mut md = format!("# {title}\n\n| | |\n|---|---|\n| Session | `{session_id}` |\n| Model | `{model}` |\n| Folder | `{cwd}` |\n\n");

    // Tool results carry only the call id; the name is on the call.
    let mut tool_names: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for m in history {
        match m.role {
            flashagent_llm::Role::System => {}
            flashagent_llm::Role::User => {
                md.push_str("---\n\n## You\n\n");
                let prompt = extract_user_prompt(&m.content);
                if !prompt.is_empty() {
                    md.push_str(prompt);
                    md.push_str("\n\n");
                }
                if !m.images.is_empty() {
                    let n = m.images.len();
                    md.push_str(&format!("*[{n} image{} attached]*\n\n", if n == 1 { "" } else { "s" }));
                }
                md.push_str("## FlashAgent\n\n");
            }
            flashagent_llm::Role::Assistant => {
                if let Some(thinking) = m.reasoning.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
                    md.push_str(&details("Thinking", thinking));
                }
                if !m.content.trim().is_empty() {
                    md.push_str(m.content.trim());
                    md.push_str("\n\n");
                }
                for call in &m.tool_calls {
                    tool_names.insert(&call.id, &call.name);
                }
            }
            flashagent_llm::Role::Tool => {
                let name = m.tool_call_id.as_deref().and_then(|id| tool_names.get(id)).copied().unwrap_or("tool");
                let mut result: String = m.content.chars().take(RESULT_CHARS).collect();
                if m.content.chars().count() > RESULT_CHARS {
                    result.push_str("\n… (cut here; the session file has all of it)");
                }
                md.push_str(&details(&format!("`{name}`"), &fenced(&result)));
            }
        }
    }
    md
}

fn details(summary: &str, body: &str) -> String {
    format!("<details><summary>{summary}</summary>\n\n{body}\n\n</details>\n\n")
}

/// The fence is longer than any backtick run inside, so ``` in a result
/// cannot end it early.
fn fenced(text: &str) -> String {
    let longest = text
        .split(|c| c != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}\n{}\n{fence}", text.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation() -> Vec<ChatMessage> {
        let mut call = ChatMessage::assistant("Let me look.");
        call.reasoning = Some("They want the file list.".into());
        call.tool_calls.push(flashagent_llm::ToolCall { id: "c1".into(), name: "list_dir".into(), args_json: "{}".into() });
        let result = ChatMessage::tool_result("c1", "src/\n```\nREADME.md");
        vec![
            ChatMessage::system("You are FlashAgent. Huge system prompt."),
            ChatMessage::user("memory: likes Rust\n\n---\n\nwhat is in this folder?"),
            call,
            result,
            ChatMessage::assistant("A `src` folder and a README."),
        ]
    }

    #[test]
    fn the_export_reads_as_the_conversation_the_user_had() {
        let md = conversation_markdown(&conversation(), "session_1_2", "qwen", "~/proj");
        assert!(md.starts_with("# what is in this folder?"), "{md}");
        assert!(!md.contains("Huge system prompt"), "the system prompt was exported");
        assert!(!md.contains("likes Rust"), "the memory block wrapped around the prompt was exported");
        assert!(md.contains("## You\n\nwhat is in this folder?\n\n## FlashAgent\n\n"), "{md}");
        assert!(md.contains("A `src` folder and a README."));
        assert!(md.contains("<details><summary>`list_dir`</summary>"), "{md}");
        assert!(md.contains("<details><summary>Thinking</summary>"));
    }

    #[test]
    fn a_result_with_backticks_cannot_break_out_of_its_fence() {
        let md = conversation_markdown(&conversation(), "s", "m", "~");
        assert!(md.contains("````\nsrc/\n```\nREADME.md\n````"), "{md}");
    }

    #[test]
    fn a_long_result_is_cut_and_says_so() {
        let mut history = conversation();
        history[3].content = "x".repeat(RESULT_CHARS + 10);
        let md = conversation_markdown(&history, "s", "m", "~");
        assert!(md.contains("cut here"));
        assert!(!md.contains(&"x".repeat(RESULT_CHARS + 1)));
    }
}
