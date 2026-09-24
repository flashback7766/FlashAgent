//! Tools for a model its server says takes none (Gemma 3, LLaVA, Phi on
//! Ollama): they are described in the system prompt and called in Hermes
//! markup, which the agent loop's [`crate::TextToolScanner`] reads back out
//! of the reply. The history is written the same way, so the model sees its
//! earlier calls and their results as text it can follow.

use crate::repair::repair_json;
use crate::types::{ChatMessage, Role, ToolSpec};

/// Appended to the system prompt.
pub(crate) fn instructions(tools: &[ToolSpec]) -> String {
    let mut text = String::from(
        "# Tools\n\n\
         You can call the tools below. To call one, write the call on a line of its own, and nothing after it:\n\
         <tool_call>{\"name\": \"<tool name>\", \"arguments\": {<arguments as a JSON object>}}</tool_call>\n\
         For several calls, one line each. Every <tool_call> you write is run, so never write one as an example. \
         The results come back in the next message, each between <tool_response> and </tool_response>.\n",
    );
    for tool in tools {
        let schema = serde_json::from_str::<serde_json::Value>(&tool.parameters_json).map(|v| v.to_string()).unwrap_or_else(|_| tool.parameters_json.clone());
        text.push_str(&format!("\n## {}\n{}\nArguments: {schema}\n", tool.name, tool.description.trim()));
    }
    text
}

/// `messages` as a model without tools can read them: the tools described in
/// the system prompt, each call written back in the markup it was made in,
/// and the results of consecutive calls in one user message.
pub(crate) fn flatten(messages: &[ChatMessage], tools: &[ToolSpec]) -> Vec<ChatMessage> {
    let mut out: Vec<ChatMessage> = Vec::with_capacity(messages.len() + 1);
    let described = instructions(tools);
    match messages.first() {
        Some(first) if first.role == Role::System => {}
        _ => out.push(ChatMessage::system(described.clone())),
    }
    let mut results_open = false;
    for (i, m) in messages.iter().enumerate() {
        match m.role {
            Role::System if i == 0 => out.push(ChatMessage::system(format!("{}\n\n{described}", m.content))),
            Role::Assistant if !m.tool_calls.is_empty() => {
                let mut msg = m.clone();
                for call in std::mem::take(&mut msg.tool_calls) {
                    let arguments = repair_json(&call.args_json)
                        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                        .unwrap_or_else(|| serde_json::json!({}));
                    let markup = serde_json::json!({ "name": call.name, "arguments": arguments });
                    if !msg.content.is_empty() && !msg.content.ends_with('\n') {
                        msg.content.push('\n');
                    }
                    msg.content.push_str(&format!("<tool_call>{markup}</tool_call>"));
                }
                out.push(msg);
                results_open = false;
                continue;
            }
            Role::Tool => {
                let result = format!("<tool_response>\n{}\n</tool_response>", m.content);
                match out.last_mut().filter(|_| results_open) {
                    Some(open) => {
                        open.content.push('\n');
                        open.content.push_str(&result);
                        open.images.extend(m.images.iter().cloned());
                    }
                    None => {
                        let mut msg = ChatMessage::user(result);
                        msg.images = m.images.clone();
                        out.push(msg);
                        results_open = true;
                    }
                }
                continue;
            }
            _ => out.push(m.clone()),
        }
        results_open = false;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{ScannerEvent, TextToolScanner};
    use crate::types::ToolCall;

    fn read_file() -> ToolSpec {
        ToolSpec {
            name: "read_file".into(),
            description: "Read a file.".into(),
            parameters_json: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#.into(),
        }
    }

    #[test]
    fn the_format_the_prompt_teaches_is_the_one_the_scanner_reads() {
        let prompt = instructions(&[read_file()]);
        assert!(prompt.contains("## read_file\nRead a file.\nArguments: {\"properties\":{\"path\":{\"type\":\"string\"}},\"type\":\"object\"}"), "{prompt}");
        let written = r#"<tool_call>{"name": "read_file", "arguments": {"path": "src/main.rs"}}</tool_call>"#;
        let mut scanner = TextToolScanner::default();
        let mut events = scanner.feed(written);
        events.extend(scanner.finish());
        assert!(
            events.iter().any(|e| matches!(e, ScannerEvent::ToolCall { name, .. } if name == "read_file")),
            "{events:?}"
        );
    }

    #[test]
    fn calls_and_results_become_text_the_model_can_follow() {
        let mut call = ChatMessage::assistant("Two files.");
        call.tool_calls = vec![
            ToolCall { id: "a".into(), name: "read_file".into(), args_json: r#"{"path":"a.rs"}"#.into() },
            ToolCall { id: "b".into(), name: "read_file".into(), args_json: r#"{"path":"b.rs",}"#.into() },
        ];
        let history = [
            ChatMessage::system("You are an agent."),
            ChatMessage::user("read a.rs and b.rs"),
            call,
            ChatMessage::tool_result("a", "fn a() {}"),
            ChatMessage::tool_result("b", "fn b() {}"),
            ChatMessage::assistant("Both define one function."),
        ];
        let flat = flatten(&history, &[read_file()]);
        let roles: Vec<Role> = flat.iter().map(|m| m.role).collect();
        assert_eq!(roles, [Role::System, Role::User, Role::Assistant, Role::User, Role::Assistant]);
        assert!(flat[0].content.starts_with("You are an agent.\n\n# Tools"));
        assert_eq!(
            flat[2].content,
            "Two files.\n<tool_call>{\"arguments\":{\"path\":\"a.rs\"},\"name\":\"read_file\"}</tool_call>\n<tool_call>{\"arguments\":{\"path\":\"b.rs\"},\"name\":\"read_file\"}</tool_call>"
        );
        assert!(flat[2].tool_calls.is_empty());
        assert_eq!(flat[3].content, "<tool_response>\nfn a() {}\n</tool_response>\n<tool_response>\nfn b() {}\n</tool_response>");
    }

    #[test]
    fn a_history_without_a_system_prompt_gets_one_for_the_tools() {
        let flat = flatten(&[ChatMessage::user("hi")], &[read_file()]);
        assert_eq!(flat[0].role, Role::System);
        assert!(flat[0].content.contains("## read_file"));
        assert_eq!(flat[1].content, "hi");
    }
}
