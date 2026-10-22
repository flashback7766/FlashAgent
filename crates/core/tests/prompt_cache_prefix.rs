//! How much of the system prompt survives a prompt-cache prefix match when the
//! environment changes (workspace, model, thinking mode).
//!
//! The server keeps KV for the longest common *token* prefix. We measure the
//! common *byte* prefix here: llama.cpp tokenizes deterministically, so for a
//! stable prefix the two agree closely enough to show the shape of the win.
//!
//! The measurement is a check, not a print: the model and the thinking mode
//! are the two things a person changes in the middle of a session, and every
//! change that moved the second block of the prompt used to cost a full
//! prefill of the whole thing. Run with:
//! `cargo test -p flashagent-core --test prompt_cache_prefix -- --nocapture`

use flashagent_core::build_system_prompt;
use flashagent_core::SystemPromptConfig;

fn common_prefix(a: &str, b: &str) -> usize {
    a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count()
}

fn cfg(cwd: &str, model: &str, effort: &str) -> SystemPromptConfig {
    SystemPromptConfig::new()
        .with_cwd(cwd)
        .with_platform("linux")
        .with_model(model)
        .with_effort(effort)
        .with_subagents(true)
}

/// The share of the prompt that is the same in both.
fn reused(a: &str, b: &str) -> f64 {
    common_prefix(a, b) as f64 / b.len() as f64 * 100.0
}

const MODEL_A: &str = "FlashModel-Qwen-3.6-35B-A3B-Maxed";
const MODEL_B: &str = "FlashModel-Qwen-3.6-35B-A3B-Reasoning";

#[test]
fn changing_the_thinking_mode_keeps_the_prompt_cached() {
    // F4, or /effort: the person changes the mode mid-session. Everything
    // before the mode has to stay, or the whole prompt is prefilled again.
    let a = build_system_prompt(&cfg("/home/user/proj", MODEL_A, "high"));
    let b = build_system_prompt(&cfg("/home/user/proj", MODEL_A, "low"));
    let shared = reused(&a, &b);
    println!("the thinking mode changed: {shared:.1}% of the prompt is kept");
    assert!(
        shared >= 95.0,
        "changing the thinking mode kept only {shared:.1}% of the prompt; the mode has to be at the end, so the rest is prefilled again"
    );
}

#[test]
fn changing_the_model_keeps_the_prompt_cached() {
    // F3: the same, for the model. The name of the model is in the prompt
    // because the model is told which one it is.
    let a = build_system_prompt(&cfg("/home/user/proj", MODEL_A, "high"));
    let b = build_system_prompt(&cfg("/home/user/proj", MODEL_B, "high"));
    let shared = reused(&a, &b);
    println!("the model changed: {shared:.1}% of the prompt is kept");
    assert!(
        shared >= 95.0,
        "changing the model kept only {shared:.1}% of the prompt; the model has to be at the end, so the rest is prefilled again"
    );
}

#[test]
fn a_change_of_folder_is_not_a_case_the_cache_has_to_survive() {
    // A different folder is a different project, and it is a different
    // session: the folder is chosen on the trust screen, before the app
    // starts. There is no cache to keep, so the folder stays where it is,
    // near the top, where the model reads where it is.
    //
    // The same folder in a new session is a different matter, and there the
    // whole prompt is identical, so all of it is kept.
    let a = build_system_prompt(&cfg("/home/user/proj", MODEL_A, "high"));
    let again = build_system_prompt(&cfg("/home/user/proj", MODEL_A, "high"));
    assert_eq!(a, again, "the same folder in a new session gives the same prompt");
    assert_eq!(reused(&a, &again), 100.0);
}

#[test]
fn how_much_survives_a_change() {
    let cases = [
        ("different folder", "/home/user/proj-a", "/home/user/proj-b", MODEL_A, "high", "high"),
        ("different model", "/home/user/proj", "/home/user/proj", MODEL_A, "high", "low"),
        ("both changed", "/home/user/proj-a", "/home/user/proj-b", MODEL_B, "high", "low"),
    ];

    println!("\n{:<26}{:>9}{:>11}{:>11}", "case", "prompt B", "shared", "reused");
    println!("{}", "-".repeat(58));
    for (name, cwd_a, cwd_b, model_a, eff_a, eff_b) in cases {
        let a = build_system_prompt(&cfg(cwd_a, model_a, eff_a));
        let b = build_system_prompt(&cfg(cwd_b, model_a, eff_b));
        let shared = common_prefix(&a, &b);
        println!(
            "{name:<26}{:>9}{:>10.1}%{:>10.1}%",
            b.len(),
            shared as f64 / b.len() as f64 * 100.0,
            shared as f64 / b.len() as f64 * 100.0
        );
    }

    // What the win is worth: the fixed bulk is the prompt minus the env block.
    let one = build_system_prompt(&cfg("/a", MODEL_A, "high"));
    let tokens = flashagent_llm::count_tokens(MODEL_A, &one);
    println!("\nprompt is {} bytes / ~{} tokens", one.len(), tokens);
    println!("at ~300 prefill tok/s, a full recompute of this prompt costs ~{:.1} s",
        tokens as f64 / 300.0);
    println!("at 96% reused, a workspace change costs ~{:.2} s\n",
        tokens as f64 * 0.04 / 300.0);
}
