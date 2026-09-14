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

/// As `ready`, with extra config fields merged in on top of the baseline.
fn ready_with(home: &Home, server: &MockServer, extra: serde_json::Value) -> Term {
    home.set_up_with(&server.url, extra);
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

    quit_with_double_esc(&mut term);
}

/// Esc, then Esc again once the app has said that is what it takes.
fn quit_with_double_esc(term: &mut Term) {
    term.send(ESC);
    term.wait_for(ESC_AGAIN, WAIT);
    term.send(ESC);
    assert!(term.wait_exit(WAIT).is_some(), "a second Esc did not quit; the screen was:\n{}", term.screen());
}

const ESC_AGAIN: &str = "Press Esc again to quit";

#[test]
fn one_esc_on_an_empty_prompt_does_not_quit_and_a_second_one_does() {
    let server = MockServer::start(Vec::new());
    let home = Home::new();
    let mut term = ready(&home, &server);

    term.send(ESC);
    term.wait_for(ESC_AGAIN, WAIT);
    assert!(term.wait_exit(Duration::from_millis(500)).is_none(), "one stray Esc quit the app");

    term.send(ESC);
    let status = term.wait_exit(WAIT);
    let screen = term.screen();
    assert!(status.is_some(), "still running after the second Esc; the screen was:\n{screen}");
    assert!(!screen.contains("Session Saved"), "offered to resume a session that was not saved:\n{screen}");
    assert!(home.sessions().is_empty(), "saved an empty session: {:?}", home.sessions());
}

#[test]
fn a_late_second_esc_asks_again_instead_of_quitting() {
    let server = MockServer::start(Vec::new());
    let home = Home::new();
    let mut term = ready(&home, &server);

    term.send(ESC);
    term.wait_for(ESC_AGAIN, WAIT);
    // Longer than the double-press window: this is a new first press.
    std::thread::sleep(Duration::from_millis(2500));
    term.send(ESC);
    assert!(term.wait_exit(Duration::from_millis(800)).is_none(), "an Esc long after the first one quit");
    term.send(ESC);
    assert!(term.wait_exit(WAIT).is_some(), "a quick second press did not quit");
}

#[test]
fn a_conversation_is_saved_when_quitting_with_double_esc() {
    let server = MockServer::start(vec![Reply::Text("Hello from the mock model.".into())]);
    let home = Home::new();
    let mut term = ready(&home, &server);

    term.type_text("say hello");
    term.send(ENTER);
    term.wait_for("Hello from the mock model.", WAIT);
    // The answer is on screen before the turn has finished settling.
    term.wait_for(PROMPT, WAIT);

    quit_with_double_esc(&mut term);
    term.wait_for("Session Saved", WAIT);
    assert_eq!(home.sessions().len(), 1, "the conversation was not saved");
}

#[test]
fn sending_the_next_prompt_stops_the_recap_still_being_written() {
    let server = MockServer::start(vec![
        Reply::Text("First answer.".into()),
        Reply::Text("Second answer.".into()),
    ]);
    server.slow_side_requests(Duration::from_millis(150));
    let home = Home::new();
    let term = ready(&home, &server);

    ask(&term, "first question", "First answer.");
    // Wait until the recap for that turn is being written.
    let deadline = std::time::Instant::now() + WAIT;
    while !server.requests().iter().any(|r| !r.is_turn() && r.body.to_string().contains("conversation analyzer")) {
        assert!(std::time::Instant::now() < deadline, "no recap was asked for");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(server.side_requests_dropped(), 0);

    ask(&term, "second question", "Second answer.");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while server.side_requests_dropped() == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the recap kept being written after the next prompt was sent"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn uninstall_asks_first_and_no_keeps_the_app_running() {
    let server = MockServer::start(Vec::new());
    let home = Home::new();
    let mut term = ready(&home, &server);

    term.type_text("/uninstall");
    term.send(ENTER);
    term.wait_for("Uninstall FlashAgent", WAIT);
    term.wait_for("Close FlashAgent and remove it?", WAIT);
    term.send("n");
    term.wait_gone("Uninstall FlashAgent", WAIT);
    assert!(term.wait_exit(Duration::from_millis(500)).is_none(), "\"n\" closed the app");
}

#[test]
fn yes_on_uninstall_closes_the_app_and_hands_over_to_the_uninstaller() {
    // The test binary is a build from source, which the uninstaller refuses
    // to touch — so this runs the whole hand-over without removing anything.
    let server = MockServer::start(Vec::new());
    let home = Home::new();
    let mut term = ready(&home, &server);

    term.type_text("/uninstall");
    term.send(ENTER);
    term.wait_for("Uninstall FlashAgent", WAIT);
    term.send("y");
    assert!(term.wait_exit(WAIT).is_some(), "\"y\" did not close the app");
    term.wait_for("build from source", WAIT);
    assert!(home.path().join(".flashagent").exists(), "a refused uninstall must not delete data");
}

#[test]
fn the_next_launch_starts_in_the_mode_the_user_left_in() {
    let server = MockServer::start(Vec::new());
    let home = Home::new();
    {
        let mut term = ready(&home, &server);
        // Accept Edits → Accept All: the one mode people might expect to be
        // forgotten, and it is remembered too.
        term.send("\x1b[Z");
        term.wait_for("Permission mode set to: Accept All", WAIT);
        quit_with_double_esc(&mut term);
    }
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.config_path()).unwrap()).unwrap();
    let saved = config["permission_mode"].as_str().unwrap_or_default().to_lowercase();
    assert!(saved.contains("bypass") || saved.contains("all"), "the mode was not saved: {config}");

    let term = Term::start(&home, &["-y"], COLS, ROWS);
    term.wait_for(PROMPT, WAIT);
    term.wait_for("Accept All", WAIT);
}

#[test]
fn planning_mode_runs_a_command_that_only_reads_without_asking() {
    let server = MockServer::start(vec![shell_call("ls"), Reply::Text("Listed the folder.".into())]);
    let home = Home::new();
    std::fs::write(home.work().join("visible.txt"), "x\n").unwrap();
    let term = ready_with(&home, &server, serde_json::json!({ "permission_mode": "Planning" }));

    term.type_text("what is in this folder?");
    term.send(ENTER);
    term.wait_for("Listed the folder.", WAIT);
    let screen = term.screen();
    assert!(!screen.contains("Confirm:"), "a read-only command asked in Planning:\n{screen}");
    let turns = server.turns();
    let last = sent(turns.last().expect("the model was asked again after the command"));
    assert!(last.contains("visible.txt"), "the command's output did not reach the model: {last}");
}

#[test]
fn the_first_turn_after_startup_already_knows_the_server_said_reasoning_is_off() {
    // Startup discovers the server through one backend and sends turns
    // through another (so the turn does not wait on its own first look); a
    // b263 regression let the very first turn go out before that discovery
    // had been handed over, so it carried no reasoning setting at all even
    // when the server had reported one and the user had turned it off.
    let server = MockServer::start(vec![Reply::Text("Reasoning is off.".into())]);
    server.report_reasoning(&["off", "on"], "on");
    let home = Home::new();
    let term = ready_with(&home, &server, serde_json::json!({ "thinking_effort": "off" }));

    term.type_text("are you thinking?");
    term.send(ENTER);
    term.wait_for("Reasoning is off.", WAIT);

    let turns = server.turns();
    assert_eq!(turns.len(), 1);
    assert_eq!(
        turns[0].body["reasoning"], "off",
        "the first turn did not carry what discovery had already found: {}",
        turns[0].body
    );
}

#[test]
fn a_model_the_server_says_nothing_about_gets_no_reasoning_fields() {
    // The server can list a model without saying anything about whether it
    // reasons — unlike a model it explicitly reports cannot. Silence must
    // not be read as "off": nothing about reasoning belongs on this model's
    // turns, or a model that never asked for a specific effort would end up
    // with one invented for it.
    const LOOKALIKE: &str = "qwen3-reasoning-lookalike";
    let server = MockServer::start(vec![Reply::Text("Just answering.".into())]);
    server.add_model_the_server_says_nothing_about(LOOKALIKE);
    let home = Home::new();
    let term = ready_with(&home, &server, serde_json::json!({ "model": LOOKALIKE }));

    term.type_text("do you reason?");
    term.send(ENTER);
    term.wait_for("Just answering.", WAIT);

    let turns = server.turns();
    assert_eq!(turns.len(), 1);
    let body = &turns[0].body;
    for field in ["reasoning", "reasoning_effort", "enable_thinking", "thinking", "chat_template_kwargs", "chat_template_config"] {
        assert!(body.get(field).is_none(), "a model the server said nothing about got a {field:?} field: {body}");
    }
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

/// Shown under the composer only while a turn runs.
const RUNNING_HINT: &str = "esc to interrupt";

/// Send a prompt and wait for the turn to finish: `answer` on screen and the
/// turn over, so the next Enter starts a new turn instead of steering this one.
fn ask(term: &Term, prompt: &str, answer: &str) {
    term.type_text(prompt);
    term.send(ENTER);
    term.wait_for(answer, WAIT);
    term.wait_gone(RUNNING_HINT, WAIT);
}

/// Everything the model was sent in one request, as text to search.
fn sent(request: &support::mock_server::Request) -> String {
    request.body["messages"].to_string()
}

fn shell_call(command: &str) -> Reply {
    Reply::ToolCall {
        name: "run_shell".into(),
        arguments: serde_json::json!({ "header": "Leave a marker file", "command": command }),
    }
}

#[test]
fn a_command_runs_once_it_is_allowed() {
    let server = MockServer::start(vec![shell_call("echo marker> marker.txt"), Reply::Text("The marker is there.".into())]);
    let home = Home::new();
    let term = ready(&home, &server);

    term.type_text("leave a marker");
    term.send(ENTER);
    term.wait_for("Confirm:", WAIT);
    assert!(!home.work().join("marker.txt").exists(), "the command ran before it was allowed");
    // Allow is the button selected when the card opens.
    term.send(ENTER);
    term.wait_for("The marker is there.", WAIT);

    let marker = std::fs::read_to_string(home.work().join("marker.txt")).expect("the allowed command ran");
    assert!(marker.contains("marker"));
    assert_eq!(server.turns().len(), 2);
}

#[test]
fn a_command_that_is_denied_never_runs_and_the_model_is_told() {
    let server = MockServer::start(vec![
        shell_call("echo marker> marker.txt"),
        Reply::Text("Understood, I will not run it.".into()),
    ]);
    let home = Home::new();
    let term = ready(&home, &server);

    term.type_text("leave a marker");
    term.send(ENTER);
    term.wait_for("Confirm:", WAIT);
    term.send("d");
    term.wait_for("Understood, I will not run it.", WAIT);

    assert!(!home.work().join("marker.txt").exists(), "a denied command ran");
    let turns = server.turns();
    assert_eq!(turns.len(), 2, "the model heard back about its call");
    let messages = turns[1].body["messages"].as_array().unwrap();
    let result = messages.iter().find(|m| m["role"] == "tool").expect("a tool result for the denied call");
    assert!(
        !result["content"].as_str().unwrap_or_default().trim().is_empty(),
        "the model was told nothing about why the call did not run: {result}"
    );
}

#[test]
fn esc_during_a_turn_stops_it_and_keeps_what_was_already_said() {
    let words: Vec<String> = (1..=60).map(|i| format!("word{i}")).collect();
    let server = MockServer::start(vec![
        Reply::Slow { text: words.join(" "), per_word: Duration::from_millis(300) },
        Reply::Text("Picked up after the interruption.".into()),
    ]);
    let home = Home::new();
    let term = ready(&home, &server);

    term.type_text("count slowly");
    term.send(ENTER);
    term.wait_for("word3", WAIT);
    term.send(ESC);
    term.wait_for("Request interrupted by user", Duration::from_secs(10));
    term.wait_gone(RUNNING_HINT, WAIT);
    assert!(!term.screen().contains("word60"), "the turn ran to the end after Esc");

    ask(&term, "go on", "Picked up after the interruption.");
    let turns = server.turns();
    assert_eq!(turns.len(), 2);
    let history = sent(&turns[1]);
    assert!(history.contains("word1 "), "the words written before Esc were not kept: {history}");
    assert!(!history.contains("word60"));
}

#[test]
fn compact_replaces_the_conversation_so_far_with_a_summary() {
    let server = MockServer::start(vec![
        Reply::Text("First answer.".into()),
        Reply::Text("Second answer.".into()),
        Reply::Text("Third answer, after the summary.".into()),
    ]);
    let home = Home::new();
    let term = ready(&home, &server);

    ask(&term, "the first question", "First answer.");
    ask(&term, "the second question", "Second answer.");
    term.type_text("/compact");
    term.send(ENTER);
    term.wait_for("Context compacted", WAIT);
    ask(&term, "the third question", "Third answer, after the summary.");

    assert!(
        server.requests().iter().any(|r| sent(r).contains("technical context compaction engine")),
        "the model was never asked for a summary"
    );
    let turns = server.turns();
    assert_eq!(turns.len(), 3);
    let after = &turns[2].body["messages"];
    assert!(
        after[0]["content"].as_str().unwrap_or_default().contains("[Compacted Conversation History]"),
        "the summary is not in the system prompt: {}",
        after[0]
    );
    assert!(!sent(&turns[2]).contains("the first question"), "the summarised turns were still sent in full");
}

/// A Python that can run the MCP stub: `python3` where there is one,
/// `python` otherwise (Windows runners have only that).
fn python() -> &'static str {
    for candidate in ["python3", "python"] {
        let ok = std::process::Command::new(candidate)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if ok {
            return candidate;
        }
    }
    panic!("the MCP scenarios need Python 3 on PATH, as python3 or python");
}

/// Configure the stub MCP server for the project in `home`, with `lookup`
/// marked read-only, and return the file it logs to.
fn with_mcp_stub(home: &Home) -> std::path::PathBuf {
    let log = home.path().join("mcp-stub.log");
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/mcp_stub.py");
    let config = serde_json::json!({ "mcpServers": { "stub": {
        "command": python(),
        "args": [script.to_string_lossy()],
        "env": { "MCP_STUB_LOG": log.to_string_lossy() },
        "read_only_tools": ["lookup"],
    } } });
    std::fs::write(home.work().join(".mcp.json"), config.to_string()).unwrap();
    log
}

fn stub_log(log: &std::path::Path) -> String {
    std::fs::read_to_string(log).unwrap_or_default()
}

/// The MCP server starts in the background; a turn sent before it has listed
/// its tools would be offered none of them.
fn wait_for_mcp(log: &std::path::Path) {
    let start = std::time::Instant::now();
    while !stub_log(log).contains("listed") {
        assert!(start.elapsed() < WAIT, "the MCP stub never listed its tools; log: {:?}", stub_log(log));
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_read_only_mcp_tool_runs_without_asking() {
    let server = MockServer::start(vec![
        Reply::ToolCall { name: "mcp__stub__lookup".into(), arguments: serde_json::json!({ "key": "alpha" }) },
        Reply::Text("Alpha is known.".into()),
    ]);
    let home = Home::new();
    let log = with_mcp_stub(&home);
    let term = ready(&home, &server);
    wait_for_mcp(&log);

    // No key is pressed: a confirmation card would hold the turn until the
    // wait runs out.
    ask(&term, "look up alpha", "Alpha is known.");

    let turns = server.turns();
    assert!(
        turns[0].body["tools"].to_string().contains("mcp__stub__lookup"),
        "the model was not offered the MCP tool"
    );
    assert!(stub_log(&log).contains("call lookup"), "the call never reached the MCP server");
    assert!(sent(&turns[1]).contains("value for alpha"), "the MCP result did not reach the model");
}

#[test]
fn an_mcp_tool_that_changes_things_asks_first_and_a_refusal_never_reaches_the_server() {
    let server = MockServer::start(vec![
        Reply::ToolCall { name: "mcp__stub__delete_everything".into(), arguments: serde_json::json!({}) },
        Reply::Text("Nothing was deleted.".into()),
    ]);
    let home = Home::new();
    let log = with_mcp_stub(&home);
    let term = ready(&home, &server);
    wait_for_mcp(&log);

    term.type_text("delete everything");
    term.send(ENTER);
    term.wait_for("Confirm:", WAIT);
    term.send("d");
    term.wait_for("Nothing was deleted.", WAIT);

    assert!(!stub_log(&log).contains("call delete_everything"), "a refused call reached the MCP server");
}

#[test]
fn a_goal_runs_without_asking_reports_what_it_did_and_hands_back_the_gates() {
    let server = MockServer::start(vec![
        Reply::ToolCall {
            name: "write_file".into(),
            arguments: serde_json::json!({ "header": "Write the greeting", "path": "greeting.txt", "content": "hello\n" }),
        },
        shell_call("echo goal> goal-marker.txt"),
        Reply::Text("The greeting is written.".into()),
        // After the goal: the same kind of command must ask again.
        shell_call("echo after> after-marker.txt"),
        Reply::Text("Asked first.".into()),
    ]);
    let home = Home::new();
    let term = ready(&home, &server);

    term.type_text("/goal write a greeting file");
    term.send(ENTER);
    term.wait_for("The greeting is written.", WAIT);
    term.wait_for("Goal report:", WAIT);
    term.wait_gone(RUNNING_HINT, WAIT);

    let screen = term.screen();
    assert!(screen.contains("+ greeting.txt"), "the report does not list the file:\n{screen}");
    assert!(screen.contains("Shell commands: 1 ok"), "the report does not count the command:\n{screen}");
    assert!(screen.contains("ended its turn on its own"), "the report does not say why it stopped:\n{screen}");
    assert_eq!(std::fs::read_to_string(home.work().join("greeting.txt")).unwrap(), "hello\n");
    assert!(home.work().join("goal-marker.txt").exists(), "the goal's command did not run");

    term.type_text("leave another marker");
    term.send(ENTER);
    term.wait_for("Confirm:", WAIT);
    assert!(!home.work().join("after-marker.txt").exists(), "after the goal a command ran without asking");
    term.send("d");
    term.wait_for("Asked first.", WAIT);
}

#[test]
fn a_goal_refuses_a_dangerous_command_without_waiting_on_a_card() {
    let server = MockServer::start(vec![
        shell_call("rm -rf build && echo ran > ran.txt"),
        Reply::Text("Left it alone.".into()),
    ]);
    let home = Home::new();
    let term = ready(&home, &server);

    term.type_text("/goal clean the build folder");
    term.send(ENTER);
    term.wait_for("Left it alone.", WAIT);
    term.wait_for("Goal report:", WAIT);

    assert!(!term.screen().contains("Confirm:"), "a goal stopped on a card nobody is there to answer:\n{}", term.screen());
    assert!(!home.work().join("ran.txt").exists(), "the dangerous command ran during /goal");
    let history = sent(server.turns().last().unwrap());
    assert!(history.contains("refused during /goal"), "the model was not told why: {history}");
}

#[test]
fn a_goal_stops_at_its_step_budget_and_says_it_is_not_finished() {
    let reads: Vec<Reply> = (0..6)
        .map(|i| Reply::ToolCall {
            name: "read_file".into(),
            arguments: serde_json::json!({ "header": format!("Read the notes, pass {i}"), "path": "notes.txt" }),
        })
        .collect();
    let server = MockServer::start(reads);
    let home = Home::new();
    std::fs::write(home.work().join("notes.txt"), "nothing new\n").unwrap();
    // The limit is set in Settings → Goal, which saves it to the config.
    let term = ready_with(&home, &server, serde_json::json!({ "goal_max_steps": 2 }));

    term.type_text("/goal read the notes forever");
    term.send(ENTER);
    term.wait_for("Goal report:", WAIT);
    term.wait_gone(RUNNING_HINT, WAIT);

    let screen = term.screen();
    assert!(screen.contains("cut short by a limit"), "the report does not say the limit ended it:\n{screen}");
    assert!(server.replies_left() > 0, "the run did not stop: every scripted step was used");
}

#[test]
fn a_goal_checkpoints_what_it_changed_every_ten_steps() {
    let mut replies: Vec<Reply> = (0..10)
        .map(|i| Reply::ToolCall {
            name: "write_file".into(),
            arguments: serde_json::json!({ "header": format!("write file {i}"), "path": format!("f{i}.txt"), "content": "x\n" }),
        })
        .collect();
    replies.push(Reply::Text("Done.".into()));
    let server = MockServer::start(replies);
    let home = Home::new();
    let git = |args: &[&str]| {
        std::process::Command::new("git").args(args).current_dir(home.work()).output().expect("run git");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@example.com"]);
    git(&["config", "user.name", "T"]);
    std::fs::write(home.work().join("README.md"), "hi\n").unwrap();
    git(&["add", "README.md"]);
    git(&["commit", "-q", "-m", "initial"]);
    let term = ready(&home, &server);

    term.type_text("/goal write ten files");
    term.send(ENTER);
    term.wait_for("checkpoint:", WAIT);
    term.wait_for("Done.", WAIT);

    let log = std::process::Command::new("git").args(["log", "--oneline"]).current_dir(home.work()).output().unwrap();
    let log = String::from_utf8_lossy(&log.stdout);
    assert!(log.contains("goal checkpoint"), "no checkpoint commit was made:\n{log}");
    assert_eq!(log.lines().count(), 2, "one checkpoint on top of the initial commit:\n{log}");
}

#[test]
fn a_goal_shows_its_live_plan_and_updates_it_in_place() {
    let plan_step = |a: &str, b: &str| {
        Reply::ToolCall {
            name: "update_plan".into(),
            arguments: serde_json::json!({ "steps": [
                { "text": "read the failing test", "status": a },
                { "text": "fix it", "status": b },
            ] }),
        }
    };
    let server = MockServer::start(vec![
        plan_step("in_progress", "pending"),
        plan_step("completed", "in_progress"),
        Reply::Text("Done.".into()),
    ]);
    let home = Home::new();
    let term = ready(&home, &server);

    term.type_text("/goal fix the failing test");
    term.send(ENTER);
    term.wait_for("Done.", WAIT);

    let screen = term.screen();
    assert!(screen.contains("plan: 1/2"), "{screen}");
    assert!(screen.contains("[x] read the failing test"), "{screen}");
    assert!(screen.contains("[~] fix it"), "{screen}");
    assert_eq!(screen.matches("plan:").count(), 1, "the plan updates in place, it does not pile up:\n{screen}");
}

#[test]
fn an_answer_cut_off_by_the_output_limit_is_finished_in_the_same_message() {
    let server = MockServer::start(vec![
        Reply::Cut("The first half of the answer, ".into()),
        Reply::Text("and the second half of it.".into()),
        Reply::Text("Second answer.".into()),
    ]);
    let home = Home::new();
    let term = ready(&home, &server);

    ask(&term, "explain it", "the second half of it.");
    assert!(
        term.screen().contains("The first half of the answer, and the second half of it."),
        "the rest was not written into the same message:\n{}",
        term.screen()
    );

    // The request for the rest carries the answer so far and asks to go on.
    let turns = server.turns();
    assert_eq!(turns.len(), 2, "one request for the answer, one for the rest");
    let messages = turns[1].body["messages"].as_array().unwrap();
    let n = messages.len();
    assert_eq!(messages[n - 2]["role"], "assistant");
    assert_eq!(messages[n - 2]["content"], "The first half of the answer, ");
    assert_eq!(messages[n - 1]["role"], "user");
    assert!(messages[n - 1]["content"].as_str().unwrap_or_default().contains("cut off"));

    // The next turn sees one whole answer, and not the request to go on.
    ask(&term, "and now?", "Second answer.");
    let later = sent(&server.turns()[2]);
    assert!(later.contains("The first half of the answer, and the second half of it."), "{later}");
    assert!(!later.contains("cut off by the output length limit"), "the request to go on was stored: {later}");
}

#[test]
fn several_files_are_read_and_changed_in_one_call_each() {
    let server = MockServer::start(vec![
        Reply::ToolCall {
            name: "read_file".into(),
            arguments: serde_json::json!({ "header": "Read both notes", "paths": ["one.txt", "two.txt"] }),
        },
        Reply::ToolCall {
            name: "edit_file".into(),
            arguments: serde_json::json!({ "header": "Finish both notes", "files": [
                { "path": "one.txt", "edits": [ { "old_string": "draft", "new_string": "final" } ] },
                { "path": "two.txt", "edits": [ { "old_string": "draft", "new_string": "final" } ] }
            ] }),
        },
        Reply::Text("Both notes are final.".into()),
    ]);
    let home = Home::new();
    std::fs::write(home.work().join("one.txt"), "first draft\n").unwrap();
    std::fs::write(home.work().join("two.txt"), "second draft\n").unwrap();
    let term = ready(&home, &server);

    ask(&term, "finish both notes", "Both notes are final.");

    assert_eq!(std::fs::read_to_string(home.work().join("one.txt")).unwrap(), "first final\n");
    assert_eq!(std::fs::read_to_string(home.work().join("two.txt")).unwrap(), "second final\n");
    let turns = server.turns();
    assert_eq!(turns.len(), 3, "one call to read both, one to change both, one to answer");
    let tool_result = |turn: &support::mock_server::Request| -> String {
        turn.body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|m| m["role"] == "tool")
            .map(|m| m["content"].as_str().unwrap_or_default().to_string())
            .unwrap_or_default()
    };
    let read = tool_result(&turns[1]);
    for part in ["=== one.txt ===", "first draft", "=== two.txt ===", "second draft"] {
        assert!(read.contains(part), "the read did not hand back {part:?}: {read}");
    }
    assert!(tool_result(&turns[2]).contains("2 file(s)"), "{}", tool_result(&turns[2]));
}

#[test]
fn an_edit_written_in_another_agents_argument_names_still_edits_the_file() {
    // OpenCode's names, one edit given flat: a model trained on another agent
    // should not fail the call over what it calls the arguments.
    let server = MockServer::start(vec![
        Reply::ToolCall {
            name: "edit_file".into(),
            arguments: serde_json::json!({ "filePath": "notes.txt", "oldString": "draft", "newString": "final" }),
        },
        Reply::Text("Edited.".into()),
    ]);
    let home = Home::new();
    let notes = home.work().join("notes.txt");
    std::fs::write(&notes, "a draft\n").unwrap();
    let term = ready(&home, &server);

    ask(&term, "finish the notes", "Edited.");

    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "a final\n", "the edit did not run");
    let turns = server.turns();
    let result = turns[1].body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("the call's result went back to the model")
        .clone();
    assert!(!result["content"].as_str().unwrap_or_default().contains("bad arguments"), "the call was rejected: {result}");
}

#[test]
fn rewind_takes_the_files_and_the_conversation_back_to_before_a_turn() {
    let edit = |old: &str, new: &str| Reply::ToolCall {
        name: "edit_file".into(),
        arguments: serde_json::json!({
            "header": "Change the notes",
            "path": "notes.txt",
            "edits": [ { "old_string": old, "new_string": new } ]
        }),
    };
    let server = MockServer::start(vec![
        edit("original", "first"),
        Reply::Text("Changed it once.".into()),
        edit("first", "second"),
        Reply::ToolCall {
            name: "write_file".into(),
            arguments: serde_json::json!({ "header": "Add a file", "path": "added.txt", "content": "new\n" }),
        },
        Reply::Text("Changed it twice.".into()),
        Reply::Text("Asked again.".into()),
    ]);
    let home = Home::new();
    let notes = home.work().join("notes.txt");
    let added = home.work().join("added.txt");
    std::fs::write(&notes, "original\n").unwrap();
    let term = ready(&home, &server);

    ask(&term, "change the notes once", "Changed it once.");
    ask(&term, "change them again", "Changed it twice.");
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "second\n");
    assert!(added.exists());

    term.type_text("/rewind");
    term.send(ENTER);
    term.wait_for("Type /rewind <number>", WAIT);

    term.type_text("/rewind 2");
    term.send(ENTER);
    // The composer morphs into a confirmation card naming what will change,
    // before anything actually does.
    term.wait_for("Confirm Rewind", WAIT);
    term.wait_for("notes.txt", WAIT);
    term.wait_for("added.txt", WAIT);
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "second\n", "the card must not touch files before it is answered");
    assert!(added.exists(), "the card must not touch files before it is answered");

    term.send(ENTER); // "Yes, rewind" is selected by default
    // The rewound turn drops off the screen; no confirmation line is shown.
    term.wait_gone("Changed it twice.", WAIT);
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "first\n", "the edit of the taken-back turn was not undone");
    assert!(!added.exists(), "a file the taken-back turn created is still there");

    // Its prompt is back in the input: sending it asks again, and the model
    // is not sent the turn that was taken back.
    term.send(ENTER);
    term.wait_for("Asked again.", WAIT);
    let turns = server.turns();
    let last = sent(turns.last().unwrap());
    assert!(last.contains("change them again"), "the prompt put back in the input is not what was sent: {last}");
    assert!(last.contains("Changed it once."), "the turn before the rewind point was lost: {last}");
    assert!(!last.contains("Changed it twice."), "the taken-back answer was still sent to the model: {last}");
}

#[test]
fn declining_the_rewind_card_leaves_everything_as_it_is() {
    let edit = |old: &str, new: &str| Reply::ToolCall {
        name: "edit_file".into(),
        arguments: serde_json::json!({
            "header": "Change the notes",
            "path": "notes.txt",
            "edits": [ { "old_string": old, "new_string": new } ]
        }),
    };
    let server = MockServer::start(vec![edit("original", "first"), Reply::Text("Changed it.".into())]);
    let home = Home::new();
    let notes = home.work().join("notes.txt");
    std::fs::write(&notes, "original\n").unwrap();
    let term = ready(&home, &server);

    ask(&term, "change the notes", "Changed it.");
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "first\n");

    term.type_text("/rewind 1");
    term.send(ENTER);
    term.wait_for("Confirm Rewind", WAIT);

    // Move off "Yes, rewind" onto "No, cancel", then confirm that.
    term.send("\x1b[B"); // Down
    term.send(ENTER);
    term.wait_gone("Confirm Rewind", WAIT);

    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "first\n", "declining must not touch the file");
    assert!(term.screen().contains("Changed it."), "declining must not drop the turn from the conversation");
}

#[test]
fn writing_outside_the_project_asks_even_in_accept_edits() {
    let server = MockServer::start(vec![
        Reply::ToolCall {
            name: "write_file".into(),
            arguments: serde_json::json!({ "header": "Write next to the project", "path": "../outside.txt", "content": "x\n" }),
        },
        Reply::Text("Stayed inside.".into()),
    ]);
    let home = Home::new();
    let term = ready(&home, &server);

    term.type_text("write a file next to the project");
    term.send(ENTER);
    term.wait_for("Confirm:", WAIT);
    term.wait_for("outside the project", WAIT);
    let target = home.path().join("outside.txt");
    assert!(!target.exists(), "the file was written before anyone answered");
    term.send("d");
    term.wait_for("Stayed inside.", WAIT);
    assert!(!target.exists(), "a denied write outside the project still happened");
}

#[test]
fn every_answer_ends_with_a_status_line_that_names_the_cache() {
    // The mock server reports prompt tokens but nothing about its cache, like
    // LM Studio over the network: the line says so instead of inventing a number.
    let server = MockServer::start(vec![Reply::Text("Status please.".into())]);
    let home = Home::new();
    let term = ready(&home, &server);
    ask(&term, "how did that go?", "Status please.");
    term.wait_for("cache hit n/a", WAIT);
    let screen = term.screen();
    assert!(!screen.contains("status:"), "status line should not duplicate as a message in chat:\n{screen}");
    let status = screen.lines().find(|l| l.contains("cache hit n/a")).unwrap_or_default();
    assert!(status.contains("prompt") && status.contains("cache hit n/a"), "{status}");
}

#[test]
fn continue_opens_the_latest_session_of_this_folder() {
    let server = MockServer::start(vec![
        Reply::Text("First answer.".into()),
        Reply::Text("Second answer.".into()),
    ]);
    let home = Home::new();
    {
        let mut term = ready(&home, &server);
        ask(&term, "first question", "First answer.");
        quit_with_double_esc(&mut term);
    }
    // Session ids are stamped to the second.
    std::thread::sleep(Duration::from_millis(1100));
    {
        let mut term = Term::start(&home, &["-y"], COLS, ROWS);
        term.wait_for(PROMPT, WAIT);
        ask(&term, "second question", "Second answer.");
        quit_with_double_esc(&mut term);
    }
    assert_eq!(home.sessions().len(), 2);

    let term = Term::start(&home, &["-y", "--continue"], COLS, ROWS);
    term.wait_for("Resumed session", WAIT);
    term.wait_for("Second answer.", WAIT);
    assert!(!term.screen().contains("First answer."), "--continue opened an older session:\n{}", term.screen());
}

#[test]
fn resume_without_an_id_lists_this_folders_sessions_to_pick_from() {
    let server = MockServer::start(vec![
        Reply::Text("The magic number is 7.".into()),
        Reply::Text("Yes, still 7.".into()),
    ]);
    let home = Home::new();
    {
        let mut term = ready(&home, &server);
        ask(&term, "what is the magic number?", "The magic number is 7.");
        quit_with_double_esc(&mut term);
    }

    let term = Term::start(&home, &["-y", "--resume"], COLS, ROWS);
    term.wait_for("Resume a Session", WAIT);
    term.wait_for("what is the magic number?", WAIT);
    term.send(ENTER);
    term.wait_for("Resumed session", WAIT);
    term.wait_for("The magic number is 7.", WAIT);
    ask(&term, "is it still the same?", "Yes, still 7.");
    let history = sent(server.turns().last().unwrap());
    assert!(history.contains("what is the magic number?"), "the picked session did not reach the model: {history}");
}

#[test]
fn slash_resume_switches_sessions_and_saves_the_one_that_was_open() {
    let server = MockServer::start(vec![
        Reply::Text("Old answer.".into()),
        Reply::Text("New answer.".into()),
        Reply::Text("Back in the old one.".into()),
    ]);
    let home = Home::new();
    {
        let mut term = ready(&home, &server);
        ask(&term, "old question", "Old answer.");
        quit_with_double_esc(&mut term);
    }
    std::thread::sleep(Duration::from_millis(1100));

    let mut term = Term::start(&home, &["-y"], COLS, ROWS);
    term.wait_for(PROMPT, WAIT);
    ask(&term, "new question", "New answer.");
    term.type_text("/resume");
    term.send(ENTER);
    term.wait_for("Resume a Session", WAIT);
    term.wait_for("old question", WAIT);
    term.send(ENTER);
    term.wait_for("Resumed session", WAIT);
    term.wait_for("Old answer.", WAIT);
    assert_eq!(home.sessions().len(), 2, "the session that was open was not saved before switching");

    ask(&term, "and now?", "Back in the old one.");
    let history = sent(server.turns().last().unwrap());
    assert!(history.contains("old question"), "the model did not get the resumed conversation: {history}");
    assert!(!history.contains("new question"), "the conversation switched away from leaked into the resumed one: {history}");
    quit_with_double_esc(&mut term);
}

#[test]
fn a_saved_session_comes_back_with_resume() {
    let server = MockServer::start(vec![
        Reply::Text("The magic number is 7.".into()),
        Reply::Text("Yes, still 7.".into()),
    ]);
    let home = Home::new();
    {
        let mut term = ready(&home, &server);
        ask(&term, "what is the magic number?", "The magic number is 7.");
        quit_with_double_esc(&mut term);
    }
    let session = home.sessions().pop().expect("the conversation was saved");
    let id = session.file_stem().unwrap().to_string_lossy().to_string();

    let term = Term::start(&home, &["-y", "--resume", &id], COLS, ROWS);
    term.wait_for(&format!("Resumed session '{id}'"), WAIT);
    term.wait_for("The magic number is 7.", WAIT);
    ask(&term, "is it still the same?", "Yes, still 7.");

    let turns = server.turns();
    assert_eq!(turns.len(), 2);
    let history = sent(&turns[1]);
    assert!(
        history.contains("what is the magic number?") && history.contains("The magic number is 7."),
        "the resumed conversation did not reach the model: {history}"
    );
}

#[test]
fn steering_during_turn_pins_message_until_completion_and_pivots() {
    let words: Vec<String> = (1..=6).map(|i| format!("word{i}")).collect();
    let server = MockServer::start(vec![
        Reply::Slow { text: words.join(" "), per_word: Duration::from_millis(500) },
        Reply::Text("Pivoted to user steering.".into()),
    ]);
    let home = Home::new();
    let term = ready(&home, &server);

    term.type_text("start counting");
    term.send(ENTER);
    term.wait_for("word2", WAIT);

    // Send steer directive while model is streaming:
    term.type_text("steer: change direction");
    term.send(ENTER);

    // Pinned message appears with steer queued
    term.wait_for("steer queued", WAIT);

    // Stream finishes cleanly (word6 arrives, NOT cut off!)
    term.wait_for("word6", WAIT);

    // The second turn starts automatically answering the steer directive!
    term.wait_for("Pivoted to user steering.", WAIT);
    term.wait_gone(RUNNING_HINT, WAIT);

    let screen = term.screen();
    assert!(!screen.contains("steer queued"), "steer queued indicator should unpin once injected:\n{screen}");
    assert!(screen.contains("steer: change direction"), "steering directive should be in chat:\n{screen}");

    // Verify history sent to server in turn 2 contains BOTH word6 and the steering message:
    let turns = server.turns();
    assert_eq!(turns.len(), 2);
    let history = sent(&turns[1]);
    assert!(history.contains("word6"), "turn 1 assistant response was not cut off: {history}");
    assert!(history.contains("steer: change direction"), "steering directive was sent to model: {history}");
}

#[test]
fn sending_only_an_image_submits_the_turn_with_image_part_and_no_text() {
    let server = MockServer::start(vec![
        Reply::Text("I see the picture.".into()),
    ]);
    let home = Home::new();
    let image_path = home.work().join("shot.png");
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend_from_slice(&[0, 0, 0, 13]);
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&800u32.to_be_bytes());
    bytes.extend_from_slice(&600u32.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
    std::fs::write(&image_path, &bytes).unwrap();

    let term = ready(&home, &server);

    // Simulate drag & drop (bracketed paste) of an image path:
    term.send(&format!("\x1b[200~{}\x1b[201~", image_path.display()));

    // Verify attachment banner and image placeholder appear:
    term.wait_for("shot.png 800×600", WAIT);
    term.wait_for("Enter to send image", WAIT);

    // Press Enter with empty input:
    term.send(ENTER);

    // Assistant answers:
    term.wait_for("I see the picture.", WAIT);

    // Verify chat transcript shows [shot.png 800×600]:
    let screen = term.screen();
    assert!(screen.contains("[shot.png 800×600]"), "chat transcript should show image label:\n{screen}");

    // Verify the request body to the model has NO text part and only image_url part:
    let turns = server.turns();
    assert_eq!(turns.len(), 1);
    let user_msg = turns[0].body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "user")
        .expect("user message present");
    let parts = user_msg["content"].as_array().expect("content parts array");
    assert_eq!(parts.len(), 1, "only image part present, no text: {parts:?}");
    assert_eq!(parts[0]["type"], "image_url");
}

