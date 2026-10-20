//! Measures how much of the system prompt survives a prompt-cache prefix match
//! when the environment changes (workspace, thinking mode), which is the case
//! that used to cost a full prefill on every directory change.
//!
//! The server keeps KV for the longest common *token* prefix. We measure the
//! common *byte* prefix here: llama.cpp tokenizes deterministically, so for a
//! stable prefix the two agree closely enough to show the shape of the win.
//! Run with: cargo test -p flashagent-core --test prompt_cache_prefix -- --nocapture

use flashagent_core::build_system_prompt;
use flashagent_core::SystemPromptConfig;

fn common_prefix(a: &str, b: &str) -> usize {
    a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count()
}

fn cfg(cwd: &str, effort: &str) -> SystemPromptConfig {
    SystemPromptConfig::new()
        .with_cwd(cwd)
        .with_platform("linux")
        .with_model("FlashModel-Qwen-3.6-35B-A3B-Maxed")
        .with_effort(effort)
        .with_subagents(true)
}

#[test]
fn how_much_survives_a_change() {
    let cases = [
        ("different workspace", "/home/user/proj-a", "/home/user/proj-b", "high", "high"),
        ("different thinking mode", "/home/user/proj", "/home/user/proj", "high", "low"),
        ("both changed", "/home/user/proj-a", "/home/user/proj-b", "high", "low"),
    ];

    println!("\n{:<26}{:>9}{:>11}{:>11}", "case", "prompt B", "shared", "reused");
    println!("{}", "-".repeat(58));
    for (name, cwd_a, cwd_b, eff_a, eff_b) in cases {
        let a = build_system_prompt(&cfg(cwd_a, eff_a));
        let b = build_system_prompt(&cfg(cwd_b, eff_b));
        let shared = common_prefix(&a, &b);
        println!(
            "{name:<26}{:>9}{:>10.1}%{:>10.1}%",
            b.len(),
            shared as f64 / b.len() as f64 * 100.0,
            shared as f64 / b.len() as f64 * 100.0
        );
    }

    // What the win is worth: the fixed bulk is the prompt minus the env block.
    let one = build_system_prompt(&cfg("/a", "high"));
    println!("\nprompt is {} bytes / ~{} tokens", one.len(), one.len() / 4);
    println!("at ~300 prefill tok/s, a full recompute of this prompt costs ~{:.1} s",
        one.len() as f64 / 4.0 / 300.0);
    println!("at 96% reused, a workspace change costs ~{:.2} s\n",
        one.len() as f64 / 4.0 * 0.04 / 300.0);
}