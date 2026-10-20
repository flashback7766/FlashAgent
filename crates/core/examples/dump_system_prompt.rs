//! Dumps the fixed prefix of the system prompt to stdout, for the slot warmup.
//!
//! This is the prompt built with an empty config: no workspace, no platform, no
//! model, no Thinking Mode, no voice. Because those parts moved to the end of
//! the prompt (see `build_system_prompt`), this text is a PREFIX of whatever
//! FlashAgent sends on a real run. That is all the server needs: it keeps cached
//! KV for the longest common token prefix, so warming the slot with the fixed
//! part leaves only the environment block and the conversation to compute.
//!
//! If FlashAgent's personality is not the default, its voice line also lands at
//! the end, so this stays a valid prefix either way.
//!
//! Subagents are the exception, and they are set here on purpose. Their section
//! sits in the middle of the prompt, not at the end, so an empty config leaves
//! it out and the warm prefix stops 7035 characters in - measured, the shared
//! prefix was exactly that. Setting it makes the dump a true prefix again:
//! 14768 of the 15439 characters a real run sends.
//!
//! run: cargo run -p flashagent-core --example dump_system_prompt

fn main() {
    let prompt = flashagent_core::build_system_prompt(
        &flashagent_core::SystemPromptConfig::new().with_subagents(true));
    print!("{prompt}");
    eprintln!(
        "\n[flashagent-slot-warmup] {} bytes, ~{} tokens (fixed prefix, no env block)",
        prompt.len(),
        prompt.len() / 4
    );
}