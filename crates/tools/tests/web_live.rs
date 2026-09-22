//! `web_search` and `web_fetch` against the real internet. `#[ignore]`d so an
//! offline `cargo test` does not fail; run on purpose and by scheduled CI:
//!
//! ```bash
//! cargo test -p flashagent-tools --test web_live -- --ignored
//! ```
//!
//! They check that a real answer comes back, not what it says.

use flashagent_core::loop_::{ToolExec, ToolOutput};
use flashagent_llm::ToolCall;
use flashagent_tools::{BuiltinTools, BuiltinToolsConfig};

async fn call(name: &str, args: serde_json::Value) -> ToolOutput {
    let dir = std::env::temp_dir().join(format!("flashagent-web-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let tools = BuiltinTools::new(BuiltinToolsConfig {
        cwd: dir,
        brave_api_key: std::env::var("BRAVE_API_KEY").ok().filter(|k| !k.trim().is_empty()),
        question_gate: None,
        is_goal_mode: None,
        toolset_profile: None,
        web_enabled: Some(true),
        context_window: Some(200_000),
        mcp_manager: None,
    })
    .unwrap();
    tools
        .execute(&ToolCall { id: "t".into(), name: name.into(), args_json: args.to_string() })
        .await
}

#[tokio::test]
#[ignore = "needs the internet"]
async fn a_search_comes_back_with_results() {
    let out = call("web_search", serde_json::json!({ "query": "rust programming language" })).await;
    assert!(!out.is_error, "{}", out.content);

    assert!(out.content.starts_with("1. "), "no first result: {}", out.content);
    assert!(out.content.contains("http"), "a result without a link: {}", out.content);
    let results = out.content.lines().filter(|l| l.starts_with("http")).count();
    assert!(results >= 3, "only {results} results came back: {}", out.content);
}

#[tokio::test]
#[ignore = "needs the internet"]
async fn a_search_that_matches_nothing_says_so_instead_of_failing() {
    // "The search is broken" and "nothing matches" are different facts, and the
    // model acts differently on each.
    let out = call(
        "web_search",
        serde_json::json!({ "query": "\"qxzjvwpl zzqq nonexistent phrase 8f3a1c\"" }),
    )
    .await;
    assert!(!out.is_error, "an empty result set is not an error: {}", out.content);
}

#[tokio::test]
#[ignore = "needs the internet"]
async fn a_page_is_fetched_as_readable_text() {
    let out = call("web_fetch", serde_json::json!({ "url": "https://example.com" })).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("Example Domain"), "{}", out.content);
    assert!(!out.content.contains("<html"), "the markup was handed over raw: {}", out.content);
}

#[tokio::test]
#[ignore = "needs the internet"]
async fn a_page_that_is_not_there_is_an_error_with_its_status() {
    let out = call("web_fetch", serde_json::json!({ "url": "https://example.com/no-such-page" })).await;
    assert!(out.is_error, "a missing page must not read as a successful fetch");
    assert!(out.content.contains("404"), "{}", out.content);
}

#[tokio::test]
#[ignore = "needs the internet"]
async fn a_redirect_is_followed_to_the_page_it_points_at() {
    let out = call("web_fetch", serde_json::json!({ "url": "http://example.com" })).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("Example Domain"), "{}", out.content);
}
