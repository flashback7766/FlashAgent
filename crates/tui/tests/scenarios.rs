//! Scenario tests: the real `flashagent` binary, in a terminal, against a
//! stand-in model server, checked by what is on the screen.
//!
//! Unit tests check the parts. These check the paths a person takes, which
//! is where the last betas broke: the wizard offering the wrong models, a
//! quit dialog on a session with nothing in it, a box drawn one column short.

mod support;

use std::time::Duration;

use support::mock_server::{MockServer, Reply, MODEL};
use support::term::{Home, Term, ENTER, ESC};

const COLS: u16 = 120;
const ROWS: u16 = 40;
/// Generous: a debug build on a busy CI runner.
const WAIT: Duration = Duration::from_secs(30);
const PROMPT: &str = "Ask FlashAgent to do anything";

/// Start an app that is already set up against `server`, past the trust
/// question, and wait for it to be ready for input.
fn ready(home: &Home, server: &MockServer) -> Term {
    home.set_up(&server.url);
    let term = Term::start(home, &["-y"], COLS, ROWS);
    term.wait_for(PROMPT, WAIT);
    term
}

#[test]
fn the_wizard_sets_up_a_custom_server_and_opens_the_app() {
    let server = MockServer::start(Vec::new());
    let home = Home::new();
    // No config at all: a first run.
    let mut term = Term::start(&home, &["-y"], COLS, ROWS);

    term.wait_for("Step 1: Choose LLM Backend", WAIT);
    term.send("6");
    term.type_text(&server.url);
    term.send(ENTER);

    term.wait_for("Step 2: API Key Configuration", WAIT);
    term.send(ENTER);

    term.wait_for("Step 3: Select Default Model", WAIT);
    term.wait_for(MODEL, WAIT);
    term.send(ENTER);

    term.wait_for("Step 4: Agent Behavior & Permissions", WAIT);
    term.send(ENTER);

    term.wait_for("Step 5: Sampling Parameters & Launch", WAIT);
    term.send(ENTER);

    // The tool check runs against the chosen model, and its verdict is kept
    // in the conversation rather than printed and cleared away.
    term.wait_for(PROMPT, WAIT);
    term.wait_for("Tool-calling check:", WAIT);

    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.config_path()).expect("the wizard saved a config")).unwrap();
    assert_eq!(config["setup_completed"], true);
    assert_eq!(config["backend_url"], server.url.as_str());
    assert_eq!(config["model"], MODEL);

    term.send(ESC);
    assert!(term.wait_exit(WAIT).is_some(), "Esc on a fresh session quits");
}

#[test]
fn esc_on_an_empty_session_quits_without_asking_or_saving() {
    let server = MockServer::start(Vec::new());
    let home = Home::new();
    let mut term = ready(&home, &server);

    term.send(ESC);
    let status = term.wait_exit(WAIT);
    let screen = term.screen();
    assert!(status.is_some(), "still running after Esc; the screen was:\n{screen}");
    assert!(!screen.contains("Quit FlashAgent"), "asked about quitting a session with nothing in it:\n{screen}");
    assert!(!screen.contains("Session Saved"), "offered to resume a session that was not saved:\n{screen}");
    assert!(home.sessions().is_empty(), "saved an empty session: {:?}", home.sessions());
}

#[test]
fn a_session_with_a_conversation_asks_before_quitting() {
    let server = MockServer::start(vec![Reply::Text("Hello from the mock model.".into())]);
    let home = Home::new();
    let mut term = ready(&home, &server);

    term.type_text("say hello");
    term.send(ENTER);
    term.wait_for("Hello from the mock model.", WAIT);
    // The answer is on screen before the turn has finished settling.
    term.wait_for(PROMPT, WAIT);

    term.send(ESC);
    term.wait_for("Quit FlashAgent", WAIT);
    term.send("n");
    term.wait_gone("Quit FlashAgent", WAIT);
    assert!(term.wait_exit(Duration::from_millis(500)).is_none(), "\"n\" quit anyway");

    term.send(ESC);
    term.wait_for("Quit FlashAgent", WAIT);
    term.send("y");
    assert!(term.wait_exit(WAIT).is_some(), "\"y\" did not quit");
    term.wait_for("Session Saved", WAIT);
    assert_eq!(home.sessions().len(), 1, "the conversation was not saved");
}

#[test]
fn the_model_list_is_not_asked_for_while_the_model_is_answering() {
    // Longer than the 15 s between looks at the server, so a look would
    // have been due at least once during the answer.
    let words: Vec<String> = (1..=17).map(|i| format!("word{i}")).collect();
    let server = MockServer::start(vec![Reply::Slow { text: words.join(" "), per_word: Duration::from_secs(1) }]);
    let home = Home::new();
    let term = ready(&home, &server);

    term.type_text("take your time");
    term.send(ENTER);
    term.wait_for("word17", Duration::from_secs(60));

    let requests = server.requests();
    let turn_started = requests.iter().find(|r| r.is_turn()).expect("the turn reached the server").at;
    let answered = std::time::Instant::now();
    let looks: Vec<_> = requests
        .iter()
        .filter(|r| r.method == "GET" && r.path.ends_with("/models") && r.at > turn_started && r.at < answered)
        .map(|r| format!("{} at +{:.1}s", r.path, (r.at - turn_started).as_secs_f32()))
        .collect();
    assert!(looks.is_empty(), "asked the server for its models while the model was answering: {looks:?}");
}

#[test]
fn a_tool_turn_runs_the_tool_and_hands_its_result_back_to_the_model() {
    let server = MockServer::start(vec![
        Reply::ToolCall {
            name: "read_file".into(),
            arguments: serde_json::json!({ "header": "Read the notes file", "path": "notes.txt" }),
        },
        Reply::Text("The notes say the code is 4217.".into()),
    ]);
    let home = Home::new();
    std::fs::write(home.work().join("notes.txt"), "the code is 4217\n").unwrap();
    let term = ready(&home, &server);

    term.type_text("what do the notes say?");
    term.send(ENTER);
    term.wait_for("Read the notes file", WAIT);
    term.wait_for("The notes say the code is 4217.", WAIT);

    let turns = server.turns();
    assert_eq!(turns.len(), 2, "one request for the call, one with its result");
    let messages = turns[1].body["messages"].as_array().expect("messages");
    let tool_result = messages
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap_or_else(|| panic!("no tool result went back to the model: {messages:#?}"));
    assert!(
        tool_result["content"].as_str().unwrap_or_default().contains("the code is 4217"),
        "the file's content did not reach the model: {tool_result}"
    );
    assert_eq!(server.replies_left(), 0);
}
