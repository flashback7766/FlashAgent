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

#[test]
fn a_path_written_with_a_tilde_is_the_file_it_means_at_a_prompt() {
    let server = MockServer::start(vec![
        Reply::ToolCall {
            name: "read_file".into(),
            // No shell runs a tool call, so nothing expands this `~` unless
            // the app does. It used to look for a folder named `~` inside
            // the project and report that the file was not there.
            arguments: serde_json::json!({ "header": "Read the notes file", "path": "~/work/notes.txt" }),
        },
        Reply::Text("The notes say the code is 4217.".into()),
    ]);
    let home = Home::new();
    std::fs::write(home.work().join("notes.txt"), "the code is 4217\n").unwrap();
    let term = ready(&home, &server);

    term.type_text("what do the notes say?");
    term.send(ENTER);
    term.wait_for("The notes say the code is 4217.", WAIT);

    let messages = server.turns()[1].body["messages"].as_array().expect("messages").clone();
    let result = messages.iter().find(|m| m["role"] == "tool").expect("a tool result went back");
    let text = result["content"].as_str().unwrap_or_default();
    assert!(text.contains("the code is 4217"), "the home folder was not where the file was looked for: {text}");
}

#[test]
fn a_tilde_path_out_of_the_project_is_still_asked_about() {
    let server = MockServer::start(vec![
        Reply::ToolCall {
            name: "read_file".into(),
            arguments: serde_json::json!({ "header": "Read the private file", "path": "~/private.txt" }),
        },
        Reply::Text("Read it.".into()),
    ]);
    let home = Home::new();
    std::fs::write(home.path().join("private.txt"), "not the project's business\n").unwrap();
    let term = ready(&home, &server);

    term.type_text("read my private file");
    term.send(ENTER);
    // The file sits next to the project, not in it: expanding the `~` must
    // not make it look like an ordinary project file that needs no question.
    term.wait_for("Confirm:", WAIT);
}

/// Every 24-bit colour on screen, as `(r, g, b)`.
fn colours_on_screen(term: &Term) -> Vec<(u8, u8, u8)> {
    let painted = term.screen_colours();
    let mut out = Vec::new();
    for piece in painted.split("38;2;").skip(1) {
        let head = piece.split('m').next().unwrap_or("");
        let parts: Vec<_> = head.split(';').take(3).filter_map(|p| p.parse::<u8>().ok()).collect();
        if let [r, g, b] = parts[..] {
            out.push((r, g, b));
        }
    }
    out
}

#[test]
fn the_colour_theme_repaints_the_whole_screen() {
    // The app is written in one palette and recoloured on the way out, so a
    // theme must reach every card without any of them knowing about it.
    let server = MockServer::start(vec![Reply::Text("Hello.".into())]);
    let home = Home::new();
    let term = ready_with(&home, &server, serde_json::json!({ "color_theme": "monochrome" }));
    term.type_text("say hello");
    term.send(ENTER);
    term.wait_for("Hello.", WAIT);

    let colours = colours_on_screen(&term);
    assert!(colours.len() > 5, "the screen should be painted at all: {colours:?}");
    let coloured: Vec<_> = colours.iter().filter(|(r, g, b)| r != g || g != b).collect();
    assert!(coloured.is_empty(), "monochrome left colours on screen: {coloured:?}");

    // And the default theme is the palette as written, so the same screen
    // does have colour in it.
    let plain_server = MockServer::start(vec![Reply::Text("Hello.".into())]);
    let plain_home = Home::new();
    let plain = ready(&plain_home, &plain_server);
    plain.type_text("say hello");
    plain.send(ENTER);
    plain.wait_for("Hello.", WAIT);
    let plain_colours = colours_on_screen(&plain);
    assert!(
        plain_colours.iter().any(|(r, g, b)| r != g || g != b),
        "the default theme should not be grey: {plain_colours:?}"
    );
}

/// A terminal the size of a small window in a tiling setup, and a very small
/// one besides: 98x21 is a quarter-screen terminal, 60x14 an editor panel.
const SMALL: (u16, u16) = (98, 21);
const TINY: (u16, u16) = (60, 14);

/// Every screen the app can put up, opened in a small terminal: what it
/// takes to get there, a word that proves it is there, and the way out it
/// prints last of all.
///
/// Waiting for that last line matters: a card unfolds over a few frames, and
/// a screen read while it is still opening is half a card.
const SCREENS: &[(&str, &str, &str)] = &[
    ("/settings", "Settings", "Esc save"),
    ("/memory", "remembers", "esc — close"),
    ("/mcp", "MCP", "Esc close"),
    // Printed into the transcript rather than put up as a screen, so the
    // marker is its last line: in a short terminal the first has scrolled
    // away by then, which is what a transcript is supposed to do.
    ("/help", "/uninstall", "Esc"),
];

fn lines_of(term: &Term) -> Vec<String> {
    term.screen().lines().map(str::to_string).collect()
}

#[test]
fn nothing_is_drawn_outside_a_small_terminal() {
    let server = MockServer::start(vec![Reply::Text("Short answer.".into())]);
    let home = Home::new();
    home.set_up(&server.url);
    let (cols, rows) = SMALL;
    let term = Term::start(&home, &["-y"], cols, rows);
    term.wait_for(PROMPT, WAIT);

    let too_wide: Vec<_> = lines_of(&term)
        .into_iter()
        .filter(|l| l.chars().count() > cols as usize)
        .collect();
    assert!(too_wide.is_empty(), "lines wider than the terminal: {too_wide:#?}");

    // The composer is the last thing on screen: if the welcome card pushed
    // it off, there is nowhere to type.
    let screen = term.screen();
    assert!(screen.contains(PROMPT), "the composer is off screen:\n{screen}");
}

#[test]
fn every_screen_fits_a_small_terminal() {
    for (cols, rows) in [SMALL, TINY] {
        for (command, marker, way_out) in SCREENS {
            let server = MockServer::start(vec![Reply::Text("ok".into())]);
            let home = Home::new();
            home.set_up(&server.url);
            let term = Term::start(&home, &["-y"], cols, rows);
            term.wait_for(PROMPT, WAIT);

            term.type_text(command);
            term.send(ENTER);
            term.wait_for(marker, WAIT);
            // The way out is the last line the card draws, so waiting for it
            // waits for the whole card.
            term.wait_for(way_out, WAIT);

            let lines = lines_of(&term);
            let too_wide: Vec<_> =
                lines.iter().filter(|l| l.chars().count() > cols as usize).cloned().collect();
            assert!(
                too_wide.is_empty(),
                "{command} at {cols}x{rows} drew past the right edge: {too_wide:#?}"
            );
            assert!(
                lines.len() <= rows as usize,
                "{command} at {cols}x{rows} drew {} rows",
                lines.len()
            );
            term.send(ESC);
        }
    }
}

#[test]
fn what_a_running_tool_is_doing_is_said_once() {
    // It used to be said twice: once on the transcript line, and again
    // inside the composer, where it read as something the user had typed.
    let header = "Check the marker file";
    // Long enough to still be running when the screen is read, in whichever
    // shell this platform gives the tool.
    let slow = if cfg!(windows) { "ping -n 4 127.0.0.1 > nul" } else { "sleep 3" };
    let server = MockServer::start(vec![
        Reply::ToolCall {
            name: "run_shell".into(),
            arguments: serde_json::json!({ "header": header, "command": slow }),
        },
        Reply::Text("Done.".into()),
    ]);
    let home = Home::new();
    // Bypass, so the command runs instead of stopping on an approval card:
    // what is being checked is the screen while a tool is working.
    let term = ready_with(&home, &server, serde_json::json!({ "permission_mode": "Bypass" }));

    term.type_text("check the marker");
    term.send(ENTER);
    term.wait_for(header, WAIT);

    let screen = term.screen();
    assert_eq!(
        screen.matches(header).count(),
        1,
        "the header is on screen twice:\n{screen}"
    );
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
    // A repaint can be caught half-written; wait for the finished frame.
    term.wait_for("[~] fix it", WAIT);

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
fn a_failed_rewind_keeps_history_and_can_be_retried() {
    let server = MockServer::start(vec![
        Reply::ToolCall {
            name: "write_file".into(),
            arguments: serde_json::json!({ "path": "notes.txt", "content": "changed" }),
        },
        Reply::Text("Notes changed.".into()),
    ]);
    let home = Home::new();
    let notes = home.work().join("notes.txt");
    std::fs::write(&notes, "original").unwrap();
    let term = ready(&home, &server);
    ask(&term, "change notes", "Notes changed.");
    std::fs::remove_file(&notes).unwrap();
    std::fs::create_dir(&notes).unwrap();
    term.type_text("/rewind 1");
    term.send(ENTER);
    term.wait_for("Confirm Rewind", WAIT);
    term.send(ENTER);
    term.wait_for("Rewind incomplete", WAIT);
    assert!(term.screen().contains("Notes changed."), "failed rewind lost the conversation");
    std::fs::remove_dir(&notes).unwrap();
    term.type_text("/rewind 1");
    term.send(ENTER);
    term.wait_for("Confirm Rewind", WAIT);
    term.send(ENTER);
    term.wait_gone("Notes changed.", WAIT);
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "original");
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
fn an_ambiguous_patch_leaves_the_file_unchanged_and_reports_the_error_to_the_model() {
    let server = MockServer::start(vec![
        Reply::ToolCall {
            name: "patch_file".into(),
            arguments: serde_json::json!({
                "path": "notes.txt", "patch": "@@ -2,1 +2,1 @@\n-same\n+changed\n"
            }),
        },
        Reply::Text("More context needed.".into()),
    ]);
    let home = Home::new();
    let path = home.work().join("notes.txt");
    let original = "same\nother\nsame\n";
    std::fs::write(&path, original).unwrap();
    let term = ready_with(&home, &server, serde_json::json!({ "toolset_profile": "Full" }));
    ask(&term, "patch the notes", "More context needed.");
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    let turns = server.turns();
    let result = turns[1].body["messages"].as_array().unwrap().iter().find(|m| m["role"] == "tool").unwrap();
    assert!(result["content"].as_str().unwrap().contains("matches multiple locations"), "{result}");
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
#[cfg(unix)]
fn a_write_through_a_symlink_parent_asks_before_touching_the_external_file() {
    let server = MockServer::start(vec![
        Reply::ToolCall {
            name: "write_file".into(),
            arguments: serde_json::json!({ "path": "link/../outside.txt", "content": "changed" }),
        },
        Reply::Text("Write refused.".into()),
    ]);
    let home = Home::new();
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir(outside.path().join("child")).unwrap();
    let target = outside.path().join("outside.txt");
    std::fs::write(&target, "original").unwrap();
    std::os::unix::fs::symlink(outside.path().join("child"), home.work().join("link")).unwrap();
    let term = ready(&home, &server);
    term.type_text("write through the link");
    term.send(ENTER);
    term.wait_for("Confirm:", WAIT);
    term.wait_for("outside the project", WAIT);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "original");
    term.send("d");
    term.wait_for("Write refused.", WAIT);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "original");
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
fn a_session_that_fails_to_load_is_left_as_it_was() {
    let server = MockServer::start(vec![Reply::Text("A fresh answer.".into())]);
    let home = Home::new();
    home.set_up(&server.url);
    let sessions = home.path().join(".flashagent").join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    // Cut off mid-write, the way a crash or a full disk leaves a file.
    let damaged = sessions.join("session_1_1.json");
    let bytes = br#"{"id":"session_1_1","timestamp":1,"model":"m","cwd":"~","messages":[{"role":"user","content":"the only copy of"#;
    std::fs::write(&damaged, bytes).unwrap();

    let mut term = Term::start(&home, &["-y", "--resume", "session_1_1"], COLS, ROWS);
    term.wait_for("unreadable", WAIT);
    term.wait_for(PROMPT, WAIT);
    ask(&term, "start over then", "A fresh answer.");
    quit_with_double_esc(&mut term);

    assert_eq!(std::fs::read(&damaged).unwrap(), bytes, "the damaged session was written over");
    assert_eq!(home.sessions().len(), 2, "the new conversation was not saved beside it: {:?}", home.sessions());
}

#[test]
fn two_instances_started_together_keep_their_own_sessions() {
    let server = MockServer::start(vec![Reply::Text("Noted.".into()), Reply::Text("Noted.".into())]);
    let home = Home::new();
    home.set_up(&server.url);
    // Started back to back: well inside one second, before either has saved.
    let mut first = Term::start(&home, &["-y"], COLS, ROWS);
    let mut second = Term::start(&home, &["-y"], COLS, ROWS);
    first.wait_for(PROMPT, WAIT);
    second.wait_for(PROMPT, WAIT);
    ask(&first, "the first one", "Noted.");
    ask(&second, "the second one", "Noted.");
    quit_with_double_esc(&mut first);
    quit_with_double_esc(&mut second);

    let saved: Vec<String> = home.sessions().iter().map(|p| std::fs::read_to_string(p).unwrap()).collect();
    assert_eq!(saved.len(), 2, "one session was saved over the other: {:?}", home.sessions());
    assert!(saved.iter().any(|s| s.contains("the first one")) && saved.iter().any(|s| s.contains("the second one")));
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
    std::fs::write(&image_path, png_bytes(800, 600)).unwrap();

    let term = ready(&home, &server);

    // Dropping a file on the terminal: a bracketed paste on Unix, and plain
    // typing on Windows, whose console has no bracketed paste at all.
    if cfg!(windows) {
        term.type_text(&image_path.display().to_string());
    } else {
        term.send(&format!("\x1b[200~{}\x1b[201~", image_path.display()));
        // The picture is recognised as it is dropped, before anything is sent.
        term.wait_for("shot.png 800×600", WAIT);
        term.wait_for("Enter to send image", WAIT);
    }

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


#[test]
fn a_picture_sent_with_words_carries_both() {
    let server = MockServer::start(vec![Reply::Text("Both received.".into())]);
    let home = Home::new();
    let image_path = home.work().join("shot.png");
    std::fs::write(&image_path, png_bytes(800, 600)).unwrap();
    let term = ready(&home, &server);

    term.type_text(&format!("what is wrong here {}", image_path.display()));
    term.send(ENTER);
    term.wait_for("Both received.", WAIT);

    let turns = server.turns();
    let user_msg = turns[0].body["messages"].as_array().unwrap().iter().find(|m| m["role"] == "user").unwrap();
    let parts = user_msg["content"].as_array().expect("content parts array");
    assert_eq!(parts.len(), 2, "the words and the picture both go: {parts:?}");
    assert_eq!(parts[0]["type"], "text");
    assert!(parts[0]["text"].as_str().unwrap().contains("what is wrong here"), "{parts:?}");
    assert_eq!(parts[1]["type"], "image_url");
}

#[test]
fn the_end_of_a_prompt_longer_than_the_terminal_stays_in_view() {
    let server = MockServer::start(Vec::new());
    let home = Home::new();
    let term = ready(&home, &server);

    let long = format!("{} TAILMARK", "word ".repeat(COLS as usize / 4));
    term.type_text(&long);
    term.wait_for("TAILMARK", WAIT);
}

#[test]
fn a_mistyped_command_is_caught_instead_of_being_sent_to_the_model() {
    let server = MockServer::start(vec![Reply::Text("should never be asked".into())]);
    let home = Home::new();
    let term = ready(&home, &server);

    term.type_text("/hlep");
    term.send(ENTER);
    term.wait_for("did you mean /help?", WAIT);
    assert!(server.turns().is_empty(), "a typo reached the model");
}

#[test]
fn clear_takes_the_conversation_off_the_screen() {
    let server = MockServer::start(vec![Reply::Text("An answer worth clearing.".into())]);
    let home = Home::new();
    let term = ready(&home, &server);

    term.type_text("hello");
    term.send(ENTER);
    term.wait_for("An answer worth clearing.", WAIT);
    term.wait_for(PROMPT, WAIT);
    term.type_text("/clear");
    term.send(ENTER);
    term.wait_gone("An answer worth clearing.", WAIT);
}

#[test]
fn an_approval_that_comes_up_over_open_settings_gets_the_answer() {
    let server = MockServer::start(vec![shell_call("echo marker> marker.txt"), Reply::Text("The marker is there.".into())]);
    // The model takes its time, so the settings are certainly open before the
    // approval card is drawn over them.
    server.delay_turns(Duration::from_secs(2));
    let home = Home::new();
    let term = ready(&home, &server);

    term.type_text("leave a marker");
    term.send(ENTER);
    term.send(TAB);
    term.wait_for("FlashAgent Settings", WAIT);
    // The card comes up over the settings and must be the one Enter answers.
    term.wait_for("Confirm:", WAIT);
    term.send(ENTER);
    term.wait_for("The marker is there.", WAIT);
    assert!(home.work().join("marker.txt").exists(), "Enter went to the settings under the card");
}

/// Just enough PNG for the header the composer reads the size from.
fn png_bytes(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend_from_slice(&[0, 0, 0, 13]);
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
    bytes
}

const F1: &str = "\x1bOP";
const F3: &str = "\x1bOR";
const F4: &str = "\x1bOS";
const F5: &str = "\x1b[15~";
const TAB: &str = "\t";
const DOWN: &str = "\x1b[B";
const RIGHT: &str = "\x1b[C";

#[test]
fn every_panel_opens_over_the_composer_and_esc_puts_the_composer_back() {
    let server = MockServer::start(Vec::new());
    let home = Home::new();
    let term = ready(&home, &server);

    for (key, title) in [
        (F1, "Context Window Breakdown"),
        (F3, "Select Model"),
        (F4, "Select Thinking Effort"),
        (F5, "Sampling Parameters"),
        (TAB, "FlashAgent Settings"),
    ] {
        term.send(key);
        term.wait_for(title, WAIT);
        term.wait_gone(PROMPT, WAIT);
        term.send(ESC);
        term.wait_gone(title, WAIT);
        term.wait_for(PROMPT, WAIT);
    }
}

#[test]
fn a_sampling_change_is_applied_and_saved() {
    let server = MockServer::start(Vec::new());
    let home = Home::new();
    let term = ready(&home, &server);

    term.send(F5);
    term.wait_for("Sampling Parameters", WAIT);
    // Down to Temperature, one step up, Enter applies.
    term.send(DOWN);
    term.send(RIGHT);
    term.wait_for("[ Custom ]", WAIT);
    term.send(ENTER);
    term.wait_for("Sampling parameters updated", WAIT);
    let config: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(home.config_path()).unwrap()).unwrap();
    assert_eq!(config["sampling_preset"], "Custom", "{config}");
}

#[test]
fn turning_tips_off_in_settings_takes_the_tip_line_away() {
    let server = MockServer::start(Vec::new());
    let home = Home::new();
    let term = ready(&home, &server);
    term.wait_for("Tip:", WAIT);

    term.send(TAB);
    term.wait_for("FlashAgent Settings", WAIT);
    term.send("3");
    term.wait_for("Developer Tips", WAIT);
    term.send(DOWN);
    term.send(ENTER);
    term.wait_for("Disabled", WAIT);
    term.send(ESC);
    term.wait_for("Settings saved", WAIT);
    term.wait_gone("Tip:", WAIT);
}

#[test]
fn a_question_from_the_model_is_answered_from_its_card() {
    let server = MockServer::start(vec![
        Reply::ToolCall {
            name: "ask_user".into(),
            arguments: serde_json::json!({ "question": "Which database?", "options": ["Postgres", "SQLite"] }),
        },
        Reply::Text("Going with it.".into()),
    ]);
    let home = Home::new();
    let term = ready(&home, &server);

    term.type_text("pick a database");
    term.send(ENTER);
    term.wait_for("Which database?", WAIT);
    term.send(DOWN);
    term.send(ENTER);
    term.wait_for("Going with it.", WAIT);
    let turns = server.turns();
    assert!(sent(turns.last().unwrap()).contains("SQLite"), "the model was not told the answer");
}

#[test]
fn slash_channel_asks_before_switching() {
    let server = MockServer::start(Vec::new());
    let home = Home::new();
    let term = ready(&home, &server);

    term.type_text("/channel beta");
    term.send(ENTER);
    term.wait_for("Switch release channel", WAIT);
    term.send("n");
    term.wait_for("Still on the Stable channel", WAIT);
    let config: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(home.config_path()).unwrap()).unwrap();
    assert_ne!(config["update_channel"], "beta", "switched without a yes: {config}");
}

#[test]
fn export_writes_the_conversation_next_to_the_project() {
    let server = MockServer::start(vec![Reply::Text("Worth keeping.".into())]);
    let home = Home::new();
    let term = ready(&home, &server);

    term.type_text("hello");
    term.send(ENTER);
    term.wait_for("Worth keeping.", WAIT);
    term.wait_for(PROMPT, WAIT);
    term.type_text("/export");
    term.send(ENTER);
    term.wait_for("Exported conversation to:", WAIT);
    let exported = std::fs::read_dir(home.work())
        .unwrap()
        .filter_map(Result::ok)
        .find(|e| e.file_name().to_string_lossy().ends_with(".md"))
        .expect("no export file");
    assert!(std::fs::read_to_string(exported.path()).unwrap().contains("Worth keeping."));
}

#[test]
fn a_style_chosen_in_settings_reaches_the_next_message() {
    let server = MockServer::start(vec![Reply::Text("Hi there!".into())]);
    let home = Home::new();
    let term = ready(&home, &server);

    term.send(TAB);
    term.wait_for("FlashAgent Settings", WAIT);
    term.send("7");
    term.wait_for("Base style and tone", WAIT);
    term.send(RIGHT);
    term.send(RIGHT);
    term.wait_for("Friendly — Warm and chatty", WAIT);
    term.send(ESC);
    term.wait_for("style (Friendly)", WAIT);

    term.type_text("hello");
    term.send(ENTER);
    term.wait_for("Hi there!", WAIT);
    assert!(sent(&server.turns()[0]).contains("warm and chatty"), "the style did not reach the model");
    let config: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(home.config_path()).unwrap()).unwrap();
    assert_eq!(config["personality"]["base"], "friendly", "{config}");
}

#[test]
fn the_memory_summary_is_written_by_the_model_and_a_dive_deeper_question_is_sent() {
    let server = MockServer::start(vec![Reply::Text("Here is the comparison.".into())]);
    server.answer_side_requests(
        "summarize what a coding assistant remembers",
        r#"{"overview": "You drive a BYD and teach.", "sections": [{"title": "Car", "text": "A BYD Sealion 07 with DiLink 5.0."}], "dive_deeper": ["Compare DiLink versions", "Describe the lesson plans"]}"#,
    );
    let home = Home::new();
    let memory = home.path().join(".flashagent").join("memory");
    std::fs::create_dir_all(&memory).unwrap();
    std::fs::write(
        memory.join("drives-byd.md"),
        "---\nname: drives-byd\ndescription: drives a BYD Sealion 07\nmetadata:\n  type: user\n---\n\nBYD Sealion 07, DiLink 5.0.\n",
    )
    .unwrap();
    let term = ready(&home, &server);

    term.type_text("/memory summary");
    term.send(ENTER);
    term.wait_for("Memory summary", WAIT);
    term.wait_for("A BYD Sealion 07 with DiLink 5.0.", WAIT);
    term.wait_for("Compare DiLink versions", WAIT);
    assert!(home.path().join(".flashagent").join("memory_summary.json").exists(), "the summary was not kept");

    term.send(ENTER);
    term.wait_for("Here is the comparison.", WAIT);
    assert!(sent(server.turns().last().unwrap()).contains("Compare DiLink versions"));
}

#[test]
#[ignore = "prints the screen for a person to look at"]
fn show_screens() {
    for (command, marker, _) in SCREENS {
        let server = MockServer::start(vec![Reply::Text("ok".into())]);
        let home = Home::new();
        home.set_up(&server.url);
        let term = Term::start(&home, &["-y"], 98, 21);
        term.wait_for(PROMPT, WAIT);
        term.type_text(command);
        term.send(ENTER);
        term.wait_for(marker, WAIT);
        std::thread::sleep(Duration::from_secs(2));
        println!("=== {command} ===");
        for line in term.screen().lines() {
            println!("|{line}|");
        }
    }
}

#[test]
#[ignore = "prints the screen for a person to look at"]
fn show_small_terminal() {
    for (cols, rows) in [(98u16, 21u16), (80, 18), (60, 14), (44, 10)] {
        let server = MockServer::start(vec![Reply::Text("ok".into())]);
        let home = Home::new();
        home.set_up(&server.url);
        let term = Term::start(&home, &["-y"], cols, rows);
        // The placeholder shortens with the width; its first word is always there.
        term.wait_for("Ask", WAIT);
        std::thread::sleep(Duration::from_secs(4));
        println!("=== {cols}x{rows} ===");
        for line in term.screen().lines() {
            println!("|{line}|");
        }
    }
}

const LEFT: &str = "\x1b[D";
const HOME: &str = "\x1b[H";
const ALT_ENTER: &str = "\x1b\r";
const CTRL_F: &str = "\x06";
const CTRL_W: &str = "\x17";

/// Text as a terminal delivers a paste: between bracketed-paste markers.
fn paste(term: &Term, text: &str) {
    term.send(&format!("\x1b[200~{text}\x1b[201~"));
}

/// The user message of a turn, as the JSON text the model was sent.
fn prompt_of(request: &support::mock_server::Request) -> String {
    let messages = request.body["messages"].as_array().cloned().unwrap_or_default();
    let user = messages.iter().rev().find(|m| m["role"] == "user").cloned().unwrap_or_default();
    user["content"].to_string()
}

#[test]
fn a_typo_is_fixed_where_it_is_not_at_the_end() {
    let server = MockServer::start(vec![Reply::Text("Fixed.".into())]);
    let home = Home::new();
    let term = ready(&home, &server);

    term.type_text("helo world");
    for _ in 0.." world".len() + 1 {
        term.send(LEFT);
    }
    term.type_text("l");
    term.send(HOME);
    term.type_text("say ");
    term.wait_for("say hello world", WAIT);
    term.send(ENTER);
    term.wait_for("Fixed.", WAIT);
    let prompt = prompt_of(server.turns().last().unwrap());
    assert!(prompt.contains("say hello world"), "{prompt}");
}

#[test]
fn a_prompt_can_have_several_lines() {
    let server = MockServer::start(vec![Reply::Text("Got both lines.".into()), Reply::Text("Got the paste.".into())]);
    let home = Home::new();
    let term = ready(&home, &server);

    // Alt+Enter and a backslash before Enter both start a new line.
    term.type_text("first line");
    term.send(ALT_ENTER);
    term.type_text("second line\\");
    term.send(ENTER);
    term.type_text("third line");
    term.wait_for("third line", WAIT);
    let screen = term.screen();
    let rows = |needle: &str| screen.lines().position(|l| l.contains(needle));
    assert!(
        rows("first line") < rows("second line") && rows("second line") < rows("third line"),
        "the lines are not one under another:\n{screen}"
    );
    term.send(ENTER);
    term.wait_for("Got both lines.", WAIT);
    term.wait_gone(RUNNING_HINT, WAIT);
    let prompt = prompt_of(server.turns().last().unwrap());
    assert!(prompt.contains("first line\\nsecond line\\nthird line"), "{prompt}");

    // A paste keeps its lines instead of running them together.
    paste(&term, "fn main() {\n    println!(\"hi\");\n}");
    term.wait_for("println", WAIT);
    term.send(ENTER);
    term.wait_for("Got the paste.", WAIT);
    let prompt = prompt_of(server.turns().last().unwrap());
    assert!(prompt.contains("fn main() {\\n    println!"), "{prompt}");
}

#[test]
fn ctrl_w_deletes_the_last_word() {
    let server = MockServer::start(vec![Reply::Text("Done.".into())]);
    let home = Home::new();
    let term = ready(&home, &server);

    term.type_text("keep this mistake");
    term.send(CTRL_W);
    term.wait_gone("mistake", WAIT);
    term.type_text("word");
    term.send(ENTER);
    term.wait_for("Done.", WAIT);
    assert!(prompt_of(server.turns().last().unwrap()).contains("keep this word"));
}

#[test]
fn ctrl_f_finds_an_earlier_prompt_to_send_again() {
    let server = MockServer::start(vec![
        Reply::Text("One.".into()),
        Reply::Text("Two.".into()),
        Reply::Text("Three.".into()),
    ]);
    let home = Home::new();
    let term = ready(&home, &server);
    ask(&term, "run the database migration", "One.");
    ask(&term, "list the files", "Two.");

    term.send(CTRL_F);
    term.wait_for("find in history", WAIT);
    term.type_text("migr");
    term.wait_for("run the database migration", WAIT);
    term.send(ENTER);
    term.wait_gone("find in history", WAIT);
    // Taken into the prompt to look at first, not sent behind the user's back.
    assert_eq!(server.turns().len(), 2);
    term.send(ENTER);
    term.wait_for("Three.", WAIT);
    assert!(prompt_of(server.turns().last().unwrap()).contains("run the database migration"));
}

#[test]
fn an_image_path_pasted_into_a_question_is_not_attached_to_a_later_prompt() {
    let server = MockServer::start(vec![
        Reply::ToolCall {
            name: "ask_user".into(),
            arguments: serde_json::json!({ "question": "Which database?", "options": ["Postgres", "SQLite"] }),
        },
        Reply::Text("Going with it.".into()),
    ]);
    let home = Home::new();
    let picture = home.work().join("shot.png");
    // Not a real picture; only the name and that it is not empty matter.
    std::fs::write(&picture, b"\x89PNG\r\n\x1a\n").unwrap();
    let term = ready(&home, &server);

    term.type_text("pick a database");
    term.send(ENTER);
    term.wait_for("Which database?", WAIT);
    paste(&term, &picture.display().to_string());
    std::thread::sleep(Duration::from_millis(300));
    term.send(DOWN);
    term.send(ENTER);
    term.wait_for("Going with it.", WAIT);
    term.wait_gone(RUNNING_HINT, WAIT);
    let screen = term.screen();
    assert!(
        !screen.contains("ctrl+z removes") && !screen.contains("shot.png attached"),
        "the pasted path became an attachment:\n{screen}"
    );
}

#[test]
#[ignore = "prints the composer with a long prompt for a person to look at"]
fn show_composer() {
    let server = MockServer::start(Vec::new());
    let home = Home::new();
    home.set_up(&server.url);
    let term = Term::start(&home, &["-y"], 60, 20);
    term.wait_for("Ask FlashAgent", WAIT);
    paste(&term, "Refactor this:\nfn main() {\n    let words = [\"a long line that has to wrap inside the box\"];\n}\n\nand explain why.");
    term.send(LEFT);
    term.send(LEFT);
    term.wait_for("explain", WAIT);
    std::thread::sleep(Duration::from_millis(300));
    println!("{}", term.screen());
}

/// Two looks at an idle screen `apart`, from the moment the prompt is up.
fn idle_screens(animations: bool, apart: Duration) -> (String, String) {
    let server = MockServer::start(Vec::new());
    let home = Home::new();
    let term = ready_with(&home, &server, serde_json::json!({ "animations": animations }));
    std::thread::sleep(Duration::from_millis(300));
    let first = term.screen();
    std::thread::sleep(apart);
    (first, term.screen())
}

#[test]
fn with_animations_off_an_idle_screen_holds_still() {
    // The mascot breathes and the tip types itself out when motion is on;
    // off, nothing on the screen may change while nobody does anything.
    let (first, later) = idle_screens(false, Duration::from_millis(1500));
    assert_eq!(first, later, "the screen moved with animations off");
    let (first, later) = idle_screens(true, Duration::from_millis(1500));
    assert_ne!(first, later, "this test cannot tell: nothing moves with animations on either");
}

#[test]
fn token_counters_switched_off_are_not_shown_as_zeroes() {
    let server = MockServer::start(vec![Reply::Slow { text: "one two three four".into(), per_word: Duration::from_millis(400) }]);
    let home = Home::new();
    let term = ready_with(&home, &server, serde_json::json!({ "show_tokens": false }));
    term.type_text("count slowly");
    term.send(ENTER);
    term.wait_for(RUNNING_HINT, WAIT);
    let screen = term.screen();
    assert!(!screen.contains("Tokens -"), "counters that are off were drawn:\n{screen}");
    term.wait_for("four", WAIT);
}

#[test]
fn the_permission_mode_is_never_cut_in_a_narrow_window() {
    let server = MockServer::start(Vec::new());
    let home = Home::new();
    home.set_up(&server.url);
    let term = Term::start(&home, &["-y"], 44, 10);
    term.wait_for("Ask", WAIT);
    term.wait_for("[Accept Edits]", WAIT);
    let screen = term.screen();
    let status = screen.lines().find(|l| l.contains("[Accept Edits]")).unwrap_or_default();
    assert!(!status.trim_end().ends_with("· R"), "a word was cut in half: {status:?}");
}

#[test]
fn the_mcp_panel_says_its_keys_once() {
    let server = MockServer::start(Vec::new());
    let home = Home::new();
    let term = ready(&home, &server);
    term.type_text("/mcp");
    term.send(ENTER);
    term.wait_for("Esc close", WAIT);
    let screen = term.screen().to_lowercase();
    assert_eq!(screen.matches("switch tab").count(), 1, "the keys are listed twice:\n{screen}");
}

#[test]
fn a_saved_session_is_found_by_something_said_inside_it() {
    let server = MockServer::start(vec![
        Reply::Text("The linker needed libssl-dev installed.".into()),
        Reply::Text("Docs written.".into()),
    ]);
    let home = Home::new();
    {
        let mut term = ready(&home, &server);
        ask(&term, "why does the build fail?", "libssl-dev");
        quit_with_double_esc(&mut term);
    }
    {
        let mut term = Term::start(&home, &["-y"], COLS, ROWS);
        term.wait_for(PROMPT, WAIT);
        ask(&term, "write the docs", "Docs written.");
        quit_with_double_esc(&mut term);
    }

    let term = Term::start(&home, &["-y", "--resume"], COLS, ROWS);
    term.wait_for("Resume a Session", WAIT);
    term.type_text("libssl");
    term.wait_for("(1/2", WAIT);
    let screen = term.screen();
    assert!(screen.contains("why does the build fail?"), "{screen}");
    assert!(!screen.contains("write the docs"), "a session without the word is still listed:\n{screen}");
    assert!(screen.contains("linker needed libssl"), "where it matched is not shown:\n{screen}");
    term.send(ENTER);
    term.wait_for("Resumed session", WAIT);
}
