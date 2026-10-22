//! Dumps FlashAgent's tool schemas in the exact OpenAI wire shape, so the slot
//! warmup can send the same `tools` array the real request sends.
//!
//! It matters whether the chat template renders tools into the prompt text: if it
//! does, they are part of the cached prefix like the system prompt, and warming
//! only the system prompt leaves them to be prefilled on every new session.
//!
//! run: cargo run -q -p flashagent-core --example dump_tools

fn main() {
    let tools = flashagent_core::registry::TOOLS;
    let arr: Vec<serde_json::Value> = tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": serde_json::from_str::<serde_json::Value>(&t.schema_json())
                        .unwrap_or(serde_json::json!({})),
                },
            })
        })
        .collect();
    let body = serde_json::to_string(&arr).unwrap();
    println!("{body}");
    eprintln!(
        "[flashagent-slot-warmup] {} tools, {} bytes of JSON, ~{} tokens",
        tools.len(),
        body.len(),
        flashagent_llm::count_tokens("warmup", &body)
    );
}