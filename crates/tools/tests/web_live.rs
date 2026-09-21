//! The two tools that need the internet, run against the real internet.
//!
//! Marked `#[ignore]`, so an ordinary `cargo test` does not fail because a
//! laptop is offline or a train went into a tunnel. They are run on purpose:
//!
//! ```bash
//! cargo test -p flashagent-tools --test web_live -- --ignored
//! ```
//!
//! and by CI on a schedule, which is what would have caught DuckDuckGo
//! changing its markup underneath `web_search` months before a person did.
//!
//! These check that a real answer comes back, not what is in it: what any
//! given page or search says is not this project's business, and asserting
//! on it would be a test that fails when the web changes its mind.

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

    // Results are numbered, one per line, each with the page it found.
    assert!(out.content.starts_with("1. "), "no first result: {}", out.content);
    assert!(out.content.contains("http"), "a result without a link: {}", out.content);
    let results = out.content.lines().filter(|l| l.starts_with("http")).count();
    assert!(results >= 3, "only {results} results came back: {}", out.content);
}

#[tokio::test]
#[ignore = "needs the internet"]
async fn a_search_that_matches_nothing_says_so_instead_of_failing() {
    // A query no page can match must read as an empty answer; "the search is
    // broken" and "the web knows nothing about this" are different facts and
    // the model acts differently on each.
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
    // HTML is reduced to what a person would read.
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
