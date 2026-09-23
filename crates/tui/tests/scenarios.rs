//! Scenario tests: the real `flashagent` binary in a terminal against a
//! stand-in model server, checked by what is on screen. They cover the paths
//! a person takes, where earlier betas broke.

mod support;

use std::time::Duration;

use support::mock_server::{MockServer, Reply, MODEL};
use support::term::{Home, Term, ENTER, ESC};

const COLS: u16 = 120;
const ROWS: u16 = 40;
/// Generous: a debug build on a busy CI runner.
const WAIT: Duration = Duration::from_secs(30);
const PROMPT: &str = "Ask FlashAgent to do anything";
/// The hint line under an approval card, whatever the call.
const APPROVAL: &str = "Esc deny";

/// Set up against `server`, past the trust question, ready for input.
fn ready(home: &Home, server: &MockServer) -> Term {
    home.set_up(&server.url);
    let term = Term::start(home, &["-y"], COLS, ROWS);
    term.wait_for(PROMPT, WAIT);
    term
}

/// With extra config fields merged on top.
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

    term.wait_for("Step 1: Choose your model server", WAIT);
    term.send("c");
    term.type_text(&server.url);
    term.send(ENTER);

    term.wait_for("Step 2: API key", WAIT);
    term.send(ENTER);

    term.wait_for("Step 3: Choose the model", WAIT);
    term.wait_for(MODEL, WAIT);
    term.send(ENTER);

    term.wait_for("Step 4: Permissions", WAIT);
    term.send(ENTER);

    term.wait_for("Step 5: Sampling and launch", WAIT);
    term.send(ENTER);

    // The tool-check verdict stays in the conversation instead of being cleared.
    term.wait_for(PROMPT, WAIT);
    term.wait_for("Tool-calling check:", WAIT);

    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.config_path()).expect("the wizard saved a config")).unwrap();
    assert_eq!(config["setup_completed"], true);
    assert_eq!(config["backend_url"], server.url.as_str());
    assert_eq!(config["model"], MODEL);

    quit(&mut term);
}

/// Ctrl+D on an empty prompt.
fn quit(term: &mut Term) {
    term.send(CTRL_D);
    assert!(term.wait_exit(WAIT).is_some(), "Ctrl+D did not quit; the screen was:\n{}", term.screen());
}

const CTRL_D: &str = "\x04";

#[test]
fn esc_never_quits_and_ctrl_d_does() {
    let server = MockServer::start(Vec::new());
    let home = Home::new();
    let mut term = ready(&home, &server);

    // Pressed to close a menu and then whatever is under it, as fast as a person does.
    for _ in 0..4 {
        term.send(ESC);
    }
    assert!(term.wait_exit(Duration::from_millis(800)).is_none(), "Esc quit the app");
    term.wait_for(PROMPT, WAIT);

    term.send(CTRL_D);
    let status = term.wait_exit(WAIT);
    let screen = term.screen();
    assert!(status.is_some(), "still running after Ctrl+D; the screen was:\n{screen}");
    assert!(!screen.contains("Session saved"), "offered to resume a session that was not saved:\n{screen}");
    assert!(home.sessions().is_empty(), "saved an empty session: {:?}", home.sessions());
}

#[test]
fn a_second_ctrl_c_on_an_empty_prompt_quits() {
    let server = MockServer::start(vec![Reply::Text("Hello from the mock model.".into())]);
    let home = Home::new();
    let mut term = ready(&home, &server);
    term.type_text("say hello");
    term.send(ENTER);
    term.wait_for("Hello from the mock model.", WAIT);
    term.wait_for(PROMPT, WAIT);

    // The first press copies the answer and says a second one quits.
    term.send("\x03");
    term.wait_for("press Ctrl+C again to exit", WAIT);
    term.send("\x03");
    assert!(term.wait_exit(WAIT).is_some(), "a second Ctrl+C did not quit");
}

#[test]
fn a_conversation_is_saved_when_quitting() {
    let server = MockServer::start(vec![Reply::Text("Hello from the mock model.".into())]);
    let home = Home::new();
    let mut term = ready(&home, &server);

    term.type_text("say hello");
    term.send(ENTER);
    term.wait_for("Hello from the mock model.", WAIT);
    // The answer is on screen before the turn has settled.
    term.wait_for(PROMPT, WAIT);

    quit(&mut term);
    term.wait_for("Session saved", WAIT);
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
fn the_recap_waits_until_the_user_has_gone_quiet() {
    let server = MockServer::start(vec![Reply::Text("First answer.".into())]);
    let home = Home::new();
    home.set_up(&server.url);
    let term = Term::start_with_env(&home, &["-y"], COLS, ROWS, &[("FLASHAGENT_RECAP_IDLE_SECS", "3")]);
    term.wait_for(PROMPT, WAIT);
    let recaps = || server.requests().iter().filter(|r| !r.is_turn() && r.body.to_string().contains("conversation analyzer")).count();

    ask(&term, "first question", "First answer.");
    // Typing keeps it back: the model stays free for the next question.
    for _ in 0..4 {
        std::thread::sleep(Duration::from_millis(900));
        term.send("x");
        term.send("\x7f");
    }
    assert_eq!(recaps(), 0, "the recap was asked for while the user was typing");

    let deadline = std::time::Instant::now() + WAIT;
    while recaps() == 0 {
        assert!(std::time::Instant::now() < deadline, "no recap once the user went quiet");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn ctrl_k_finds_a_command_by_what_it_is_called_or_its_key() {
    let server = MockServer::start(Vec::new());
    let home = Home::new();
    let term = ready(&home, &server);

    term.send("\x0b");
    term.wait_for("Commands", WAIT);
    // "f1" is the key of /context; typing it narrows the list to that.
    term.type_text("f1");
    term.send(ENTER);
    term.wait_for("Context usage", WAIT);
    term.send(ESC);
    term.wait_for(PROMPT, WAIT);

    // A command that needs an argument waits in the prompt for it.
    term.send("\x0b");
    term.wait_for("Commands", WAIT);
    term.type_text("goal");
    term.send(ENTER);
    term.wait_for("\u{203a} /goal", WAIT);
}

#[test]
fn enter_runs_the_command_the_popup_highlights() {
    let server = MockServer::start(Vec::new());
    let home = Home::new();
    let term = ready(&home, &server);
    term.type_text("/hel");
    term.send(ENTER);
    term.wait_for("it never quits", WAIT);
    assert!(!term.screen().contains("Unknown command"), "{}", term.screen());
}

#[test]
fn scrolling_back_moves_the_conversation_and_keeps_the_composer() {
    let story: String = (0..120).map(|i| format!("line{i}\n\n")).collect();
    let server = MockServer::start(vec![Reply::Text(story)]);
    let home = Home::new();
    let term = ready(&home, &server);
    ask(&term, "tell me a long story", "line119");

    term.send("\x1b[5~");
    let screen = term.wait_for("more lines below", WAIT);
    assert!(screen.contains(PROMPT), "the composer scrolled away:\n{screen}");
    // Typing keeps the place.
    term.type_text("a reply");
    let screen = term.wait_for("a reply", WAIT);
    assert!(screen.contains("more lines below"), "typing jumped back to the bottom:\n{screen}");
    term.send("\x1b[F");
    term.wait_gone("more lines below", WAIT);
    term.wait_for("line119", WAIT);
}

#[test]
fn a_click_on_a_thought_opens_it() {
    let server = MockServer::start(vec![Reply::Text("<think>Weighing the options carefully</think>Done thinking.".into())]);
    let home = Home::new();
    let term = ready(&home, &server);
    ask(&term, "think about it", "Done thinking.");
    let screen = term.wait_for("Thought:", WAIT);
    assert!(!screen.contains("Weighing the options"), "already open:\n{screen}");
    let row = screen.lines().position(|l| l.contains("Thought:")).unwrap();
    // SGR mouse report: left button down, then up, at column 5 of that row.
    term.write(&format!("\x1b[<0;5;{}M\x1b[<0;5;{}m", row + 1, row + 1));
    term.wait_for("Weighing the options", WAIT);
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
    // The test binary is a source build, which the uninstaller refuses to touch,
    // so this runs the whole hand-over without removing anything.
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
        // Accept All is remembered too.
        term.send("\x1b[Z");
        term.wait_for("Permission mode set to: Accept All", WAIT);
        quit(&mut term);
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
    assert!(!screen.contains(APPROVAL), "a read-only command asked in Planning:\n{screen}");
    let turns = server.turns();
    let last = sent(turns.last().expect("the model was asked again after the command"));
    assert!(last.contains("visible.txt"), "the command's output did not reach the model: {last}");
}

#[test]
fn the_first_turn_after_startup_already_knows_the_server_said_reasoning_is_off() {
    // b263 regression: the first turn went out before discovery was handed to
    // the sending backend, so it carried no reasoning setting even when the user
    // had turned reasoning off.
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
    // Silence about reasoning is not "off": no reasoning fields may be invented
    // for this model's turns.
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
    // Longer than the 15 s poll interval, so a poll was due during the answer.
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
            // Nothing expands this `~` unless the app does; it used to be looked for
            // inside the project.
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
    // The file is outside the project, so it must still be asked about.
    term.wait_for(APPROVAL, WAIT);
}

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
    // A theme must reach every card without any of them knowing about it.
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

    // The default theme is the palette as written.
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

/// 98x21 is a quarter-screen terminal, 60x14 an editor panel.
const SMALL: (u16, u16) = (98, 21);
const TINY: (u16, u16) = (60, 14);

/// (how to open it, a word proving it is there, its last line). Waiting for
/// the last line waits for the whole card to unfold.
const SCREENS: &[(&str, &str, &str)] = &[
    ("/settings", "Settings", "Esc save and close"),
    ("/memory", "remembers", "Esc close"),
    ("/mcp", "MCP", "Esc close"),
    // Printed into the transcript, so the marker is its last line; the first has
    // scrolled away in a short terminal.
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
    // The composer being there proves the welcome card did not push it off.
    let screen = term.wait_for(PROMPT, WAIT);

    let too_wide: Vec<_> = screen.lines().filter(|l| l.chars().count() > cols as usize).collect();
    assert!(too_wide.is_empty(), "lines wider than the terminal: {too_wide:#?}");
}

#[test]
fn every_screen_fits_a_small_terminal() {
    for (cols, rows) in [SMALL, TINY] {
        for (command, _title, way_out) in SCREENS {
            let server = MockServer::start(vec![Reply::Text("ok".into())]);
            let home = Home::new();
            home.set_up(&server.url);
            let term = Term::start(&home, &["-y"], cols, rows);
            term.wait_for(PROMPT, WAIT);

            term.type_text(command);
            term.send(ENTER);
            // The last line, not the title: in 14 rows a tall card shows its title only
            // while unfolding, which a slow machine can miss.
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
    // It used to appear twice, once inside the composer as if typed.
    let header = "Check the marker file";
    // Still running when the screen is read, in either platform's shell.
    let slow = if cfg!(windows) { "ping -n 4 127.0.0.1" } else { "sleep 3" };
    let server = MockServer::start(vec![
        Reply::ToolCall {
            name: "run_shell".into(),
            arguments: serde_json::json!({ "header": header, "command": slow }),
        },
        Reply::Text("Done.".into()),
    ]);
    let home = Home::new();
    // Bypass, so no approval card: the check is the screen while a tool works.
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

/// Shown only while a turn runs.
const RUNNING_HINT: &str = "Esc interrupt";

/// Waits until the turn is over, so the next Enter starts a new turn.
fn ask(term: &Term, prompt: &str, answer: &str) {
    term.type_text(prompt);
    term.send(ENTER);
    term.wait_for(answer, WAIT);
    term.wait_gone(RUNNING_HINT, WAIT);
}

/// As text to search.
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
    term.wait_for(APPROVAL, WAIT);
    assert!(!home.work().join("marker.txt").exists(), "the command ran before it was allowed");
    // Allow is selected when the card opens.
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
    term.wait_for(APPROVAL, WAIT);
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
fn a_new_file_card_keeps_indentation_and_shows_escapes_as_text() {
    let server = MockServer::start(vec![
        Reply::ToolCall {
            name: "write_file".into(),
            arguments: serde_json::json!({ "path": "new.rs", "content": "fn a() {\n    let x = 1;\n    // \u{1b}[8mhidden\u{1b}[0m\n}\n" }),
        },
        Reply::Text("Written.".into()),
    ]);
    let home = Home::new();
    let term = ready_with(&home, &server, serde_json::json!({ "permission_mode": "Manual" }));
    term.type_text("make new.rs");
    term.send(ENTER);
    term.wait_for(APPROVAL, WAIT);
    // The card unfolds; its last rows are the buttons.
    let screen = term.wait_for("Always allow", WAIT);
    assert!(screen.contains("Create this file?"), "{screen}");
    assert!(screen.contains("+     let x = 1;"), "indentation lost:\n{screen}");
    assert!(screen.contains("^[[8mhidden"), "an escape was executed instead of shown:\n{screen}");
    term.send(ESC);
}

#[test]
fn esc_clears_a_steering_draft_before_it_stops_the_turn() {
    let words: Vec<String> = (1..=60).map(|i| format!("word{i}")).collect();
    let server = MockServer::start(vec![Reply::Slow { text: words.join(" "), per_word: Duration::from_millis(300) }]);
    let home = Home::new();
    let term = ready(&home, &server);

    term.type_text("count slowly");
    term.send(ENTER);
    term.wait_for("word3", WAIT);
    term.type_text("draftsteer");
    term.wait_for("Esc clear", WAIT);
    term.send(ESC);
    term.wait_gone("draftsteer", WAIT);
    // Still running: the draft went, the turn did not.
    let screen = term.wait_for(RUNNING_HINT, WAIT);
    assert!(!screen.contains("Request interrupted by user"), "{screen}");
    term.send(ESC);
    term.wait_for("Request interrupted by user", Duration::from_secs(10));
}

#[test]
fn compact_replaces_the_conversation_so_far_with_a_summary() {
    let server = MockServer::start(vec![
        Reply::Text("First answer.".into()),
        Reply::Text("Second answer.".into()),
        Reply::Text("Third answer, after the summary.".into()),
    ]);
    server.answer_side_requests("technical context compaction engine", "Summary:\n1. Primary Request and Intent: answer the user's questions.\n7. Pending Tasks: None.\n8. Current Work: continue with the next question.");
    let home = Home::new();
    let term = ready(&home, &server);

    let first_question = format!("the first question {}", "details ".repeat(150));
    paste(&term, &first_question);
    term.send(ENTER);
    term.wait_for("First answer.", WAIT);
    term.wait_gone(RUNNING_HINT, WAIT);
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
    let archives = home.path().join(".flashagent").join("sessions").join("compactions");
    let archive = std::fs::read_dir(archives).unwrap().next().unwrap().unwrap().path();
    let saved = std::fs::read_to_string(archive).unwrap();
    assert!(saved.contains("the first question"));
    assert!(saved.contains("First answer."));
}

/// `python3`, or `python` on Windows runners.
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

/// `lookup` marked read-only. Returns the log file.
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

/// A turn sent before the server listed its tools would get none.
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

    // No key is pressed: a confirmation card would hold the turn until timeout.
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
    term.wait_for(APPROVAL, WAIT);
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
        // After the goal, the same kind of command must ask again.
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
    term.wait_for(APPROVAL, WAIT);
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

    assert!(!term.screen().contains(APPROVAL), "a goal stopped on a card nobody is there to answer:\n{}", term.screen());
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
    // Settings → Goal saves the limit to the config.
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
    // A repaint can be caught half-written; check the finished frame.
    let screen = term.wait_for("[~] fix it", WAIT);

    assert!(screen.contains("plan: 1/2"), "{screen}");
    assert!(screen.contains("[x] read the failing test"), "{screen}");
    assert!(screen.contains("[~] fix it"), "{screen}");
    assert_eq!(screen.matches("plan:").count(), 1, "the plan updates in place, it does not pile up:\n{screen}");
    // Only the text answer carries a usage chunk in the mock: four tokens.
    let report = term.wait_for("generated tokens", WAIT);
    assert!(report.contains("· 4 generated tokens"), "{report}");
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

    // The continuation request carries the answer so far.
    let turns = server.turns();
    assert_eq!(turns.len(), 2, "one request for the answer, one for the rest");
    let messages = turns[1].body["messages"].as_array().unwrap();
    let n = messages.len();
    assert_eq!(messages[n - 2]["role"], "assistant");
    assert_eq!(messages[n - 2]["content"], "The first half of the answer, ");
    assert_eq!(messages[n - 1]["role"], "user");
    assert!(messages[n - 1]["content"].as_str().unwrap_or_default().contains("cut off"));

    // The next turn sees one whole answer, and not the continuation request.
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
    // OpenCode's argument names, one edit given flat.
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
    // The confirmation card names what will change before anything does.
    term.wait_for("Confirm Rewind", WAIT);
    term.wait_for("notes.txt", WAIT);
    term.wait_for("added.txt", WAIT);
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "second\n", "the card must not touch files before it is answered");
    assert!(added.exists(), "the card must not touch files before it is answered");

    term.send(ENTER); // "Yes, rewind" is selected by default
    term.wait_gone("Changed it twice.", WAIT);
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "first\n", "the edit of the taken-back turn was not undone");
    assert!(!added.exists(), "a file the taken-back turn created is still there");

    // The prompt is back in the input; the model is not sent the taken-back turn.
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

    // Onto "No, cancel", then confirm.
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
    term.wait_for(APPROVAL, WAIT);
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
    term.wait_for(APPROVAL, WAIT);
    term.wait_for("outside the project", WAIT);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "original");
    term.send("d");
    term.wait_for("Write refused.", WAIT);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "original");
}

#[test]
fn after_an_answer_the_status_line_is_the_mode_not_a_report() {
    let server = MockServer::start(vec![Reply::Text("Status please.".into())]);
    let home = Home::new();
    // Tips are picked at random, and one says "prompt".
    let term = ready_with(&home, &server, serde_json::json!({ "show_tips": false }));
    ask(&term, "how did that go?", "Status please.");
    let screen = term.wait_for("Ready", WAIT);
    let status = screen.lines().find(|l| l.contains("Ready")).unwrap_or_default();
    assert!(status.contains("[Accept Edits]"), "{status}");
    for noise in ["cache hit", "prompt", "TTFT", "tg", "Tokens -"] {
        assert!(!screen.contains(noise), "{noise:?} is back on screen:\n{screen}");
    }
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
        quit(&mut term);
    }
    // Session ids are stamped to the second.
    std::thread::sleep(Duration::from_millis(1100));
    {
        let mut term = Term::start(&home, &["-y"], COLS, ROWS);
        term.wait_for(PROMPT, WAIT);
        ask(&term, "second question", "Second answer.");
        quit(&mut term);
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
        quit(&mut term);
    }

    let term = Term::start(&home, &["-y", "--resume"], COLS, ROWS);
    term.wait_for("Resume a session", WAIT);
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
        quit(&mut term);
    }
    std::thread::sleep(Duration::from_millis(1100));

    let mut term = Term::start(&home, &["-y"], COLS, ROWS);
    term.wait_for(PROMPT, WAIT);
    ask(&term, "new question", "New answer.");
    term.type_text("/resume");
    term.send(ENTER);
    term.wait_for("Resume a session", WAIT);
    term.wait_for("old question", WAIT);
    term.send(ENTER);
    term.wait_for("Resumed session", WAIT);
    term.wait_for("Old answer.", WAIT);
    assert_eq!(home.sessions().len(), 2, "the session that was open was not saved before switching");

    ask(&term, "and now?", "Back in the old one.");
    let history = sent(server.turns().last().unwrap());
    assert!(history.contains("old question"), "the model did not get the resumed conversation: {history}");
    assert!(!history.contains("new question"), "the conversation switched away from leaked into the resumed one: {history}");
    quit(&mut term);
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
        quit(&mut term);
    }
    let session = home.sessions().pop().expect("the conversation was saved");
    let id = session.file_stem().unwrap().to_string_lossy().to_string();

    let term = Term::start(&home, &["-y", "--resume", &id], COLS, ROWS);
    term.wait_for(&format!("Resumed session {id}"), WAIT);
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
    // Cut off mid-write, as a crash or full disk leaves it.
    let damaged = sessions.join("session_1_1.json");
    let bytes = br#"{"id":"session_1_1","timestamp":1,"model":"m","cwd":"~","messages":[{"role":"user","content":"the only copy of"#;
    std::fs::write(&damaged, bytes).unwrap();

    let mut term = Term::start(&home, &["-y", "--resume", "session_1_1"], COLS, ROWS);
    term.wait_for("unreadable", WAIT);
    term.wait_for(PROMPT, WAIT);
    ask(&term, "start over then", "A fresh answer.");
    quit(&mut term);

    assert_eq!(std::fs::read(&damaged).unwrap(), bytes, "the damaged session was written over");
    assert_eq!(home.sessions().len(), 2, "the new conversation was not saved beside it: {:?}", home.sessions());
}

#[test]
fn two_instances_started_together_keep_their_own_sessions() {
    let server = MockServer::start(vec![Reply::Text("Noted.".into()), Reply::Text("Noted.".into())]);
    let home = Home::new();
    home.set_up(&server.url);
    // Back to back, within one second, before either has saved.
    let mut first = Term::start(&home, &["-y"], COLS, ROWS);
    let mut second = Term::start(&home, &["-y"], COLS, ROWS);
    first.wait_for(PROMPT, WAIT);
    second.wait_for(PROMPT, WAIT);
    ask(&first, "the first one", "Noted.");
    ask(&second, "the second one", "Noted.");
    quit(&mut first);
    quit(&mut second);

    let saved: Vec<String> = home.sessions().iter().map(|p| std::fs::read_to_string(p).unwrap()).collect();
    assert_eq!(saved.len(), 2, "one session was saved over the other: {:?}", home.sessions());
    assert!(saved.iter().any(|s| s.contains("the first one")) && saved.iter().any(|s| s.contains("the second one")));
}

#[test]
fn steering_during_turn_pins_message_until_completion_and_pivots() {
    let words: Vec<String> = (1..=6).map(|i| format!("word{i}")).collect();
    let server = MockServer::start(vec![
        // Slow enough that a loaded CI machine sees the steer queued before the
        // answer ends.
        Reply::Slow { text: words.join(" "), per_word: Duration::from_millis(1000) },
        Reply::Text("Pivoted to user steering.".into()),
    ]);
    let home = Home::new();
    let term = ready(&home, &server);

    term.type_text("start counting");
    term.send(ENTER);
    term.wait_for("word2", WAIT);

    term.type_text("steer: change direction");
    term.send(ENTER);

    term.wait_for("steer queued", WAIT);

    // The stream finishes; it is not cut off.
    term.wait_for("word6", WAIT);

    // The next turn answers the steer.
    term.wait_for("Pivoted to user steering.", WAIT);
    term.wait_gone(RUNNING_HINT, WAIT);

    let screen = term.screen();
    assert!(!screen.contains("steer queued"), "steer queued indicator should unpin once injected:\n{screen}");
    assert!(screen.contains("steer: change direction"), "steering directive should be in chat:\n{screen}");

    // Turn 2's history holds both word6 and the steer.
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

    // A bracketed paste on Unix, plain typing on Windows.
    if cfg!(windows) {
        term.type_text(&image_path.display().to_string());
    } else {
        term.send(&format!("\x1b[200~{}\x1b[201~", image_path.display()));
        // Recognised as it is dropped, before anything is sent.
        term.wait_for("shot.png 800×600", WAIT);
        term.wait_for("Enter to send the image", WAIT);
    }

    term.send(ENTER);

    term.wait_for("I see the picture.", WAIT);

    let screen = term.screen();
    assert!(screen.contains("[shot.png 800×600]"), "chat transcript should show image label:\n{screen}");

    // The request carries only the image part, no text.
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
    // The settings are open before the approval card is drawn over them.
    server.delay_turns(Duration::from_secs(2));
    let home = Home::new();
    let term = ready(&home, &server);

    term.type_text("leave a marker");
    term.send(ENTER);
    term.send(TAB);
    term.wait_for("╭─ Settings", WAIT);
    // The card over the settings must be the one Enter answers.
    term.wait_for(APPROVAL, WAIT);
    term.send(ENTER);
    term.wait_for("The marker is there.", WAIT);
    assert!(home.work().join("marker.txt").exists(), "Enter went to the settings under the card");
}

/// Just enough for the size header.
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
const TAB: &str = "\t";
const DOWN: &str = "\x1b[B";
const RIGHT: &str = "\x1b[C";

#[test]
fn every_panel_opens_over_the_composer_and_esc_puts_the_composer_back() {
    let server = MockServer::start(Vec::new());
    let home = Home::new();
    let term = ready(&home, &server);

    for (key, title) in [
        (F1, "Context usage"),
        (F3, "Select model"),
        (F4, "Thinking effort"),
        (TAB, "╭─ Settings"),
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

    // Only for those who look for it: no key of its own.
    term.type_text("/sampling");
    term.send(ENTER);
    term.wait_for("Sampling parameters", WAIT);
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
    term.wait_for("╭─ Settings", WAIT);
    term.send("3");
    term.wait_for("Developer tips", WAIT);
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
fn typing_in_a_question_card_answers_in_ones_own_words() {
    let server = MockServer::start(vec![
        Reply::ToolCall {
            name: "ask_user".into(),
            arguments: serde_json::json!({ "question": "Which database?", "options": ["Postgres", "SQLite"] }),
        },
        Reply::Text("Redis it is.".into()),
    ]);
    let home = Home::new();
    let term = ready(&home, &server);

    term.type_text("pick a database");
    term.send(ENTER);
    term.wait_for("Which database?", WAIT);
    // No key to open the write-in first; the digit in it does not pick option 2.
    term.type_text("Redis 2");
    term.wait_for("Redis 2", WAIT);
    term.send(ENTER);
    term.wait_for("Redis it is.", WAIT);
    let turns = server.turns();
    let messages = turns.last().unwrap().body["messages"].as_array().cloned().unwrap_or_default();
    let answer = messages.iter().find(|m| m["role"] == "tool").and_then(|m| m["content"].as_str()).unwrap_or_default().to_string();
    assert!(answer.contains("Redis 2") && !answer.contains("SQLite"), "{answer}");
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
    term.wait_for("Exported to", WAIT);
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
    term.wait_for("╭─ Settings", WAIT);
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
    let sent = sent(&server.turns()[0]);
    assert!(sent.contains("friendly colleague"), "the voice did not reach the model: {sent}");
    assert!(sent.contains("Happy to help!"), "the example in that voice did not reach the model: {sent}");
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

/// Bracketed on Unix; on Windows as keys arriving together, each newline an Enter.
fn paste(term: &Term, text: &str) {
    if cfg!(windows) {
        term.write(&text.replace('\n', "\r"));
        std::thread::sleep(Duration::from_millis(100));
    } else {
        term.send(&format!("\x1b[200~{text}\x1b[201~"));
    }
}

/// As the JSON text the model was sent.
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

    // A paste keeps its lines.
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
    // Taken into the prompt, not sent behind the user's back.
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
    // Only the name and non-emptiness matter.
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

/// Two looks at an idle screen `apart`.
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
    // With motion off, nothing may change while nobody does anything.
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
    // The footer used to repeat the keys the panel shows inside its box.
    let screen = term.screen();
    let lines: Vec<&str> = screen.lines().collect();
    let bottom = lines.iter().rposition(|l| l.contains('╰')).expect("the panel's bottom edge");
    let footer = lines.get(bottom + 1).copied().unwrap_or_default().to_lowercase();
    assert!(!footer.contains("switch tab"), "the keys are listed again under the panel:\n{screen}");
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
        quit(&mut term);
    }
    {
        let mut term = Term::start(&home, &["-y"], COLS, ROWS);
        term.wait_for(PROMPT, WAIT);
        ask(&term, "write the docs", "Docs written.");
        quit(&mut term);
    }

    let term = Term::start(&home, &["-y", "--resume"], COLS, ROWS);
    term.wait_for("Resume a session", WAIT);
    term.type_text("libssl");
    term.wait_for("(1/2", WAIT);
    let screen = term.screen();
    assert!(screen.contains("why does the build fail?"), "{screen}");
    assert!(!screen.contains("write the docs"), "a session without the word is still listed:\n{screen}");
    assert!(screen.contains("linker needed libssl"), "where it matched is not shown:\n{screen}");
    term.send(ENTER);
    term.wait_for("Resumed session", WAIT);
}

/// In MB, from /proc (Linux only).
fn rss_mb(pid: u32) -> Option<f64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let kb: f64 = status.lines().find(|l| l.starts_with("VmRSS:"))?.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024.0)
}

#[test]
#[ignore = "a measurement for docs/numbers.md; run with --release"]
fn measure_startup_and_memory() {
    let server = MockServer::start(Vec::new());
    let home = Home::new();
    home.set_up(&server.url);

    let mut starts = Vec::new();
    for _ in 0..5 {
        let started = std::time::Instant::now();
        let mut term = Term::start(&home, &["-y"], COLS, ROWS);
        term.wait_for(PROMPT, WAIT);
        starts.push(started.elapsed().as_secs_f64() * 1000.0);
        std::thread::sleep(Duration::from_millis(1500));
        if let Some(mb) = term.pid().and_then(rss_mb) {
            println!("idle memory: {mb:.1} MB");
        }
        quit(&mut term);
    }
    starts.sort_by(f64::total_cmp);
    println!("time to the first usable frame: median {:.0} ms (runs: {starts:.0?})", starts[2]);

    // A long saved conversation, resumed.
    let turns = 2_000;
    let mut messages = vec![serde_json::json!({ "role": "system", "content": "sys", "reasoning": null, "tool_call_id": null, "tool_calls": [] })];
    for n in 0..turns {
        messages.push(serde_json::json!({ "role": "user", "content": format!("Question {n}: what does the parser do with an unclosed block?"), "reasoning": null, "tool_call_id": null, "tool_calls": [] }));
        messages.push(serde_json::json!({ "role": "assistant", "content": format!("Answer {n}: it keeps the text as it is and **reports the block** at the end.\n\n- one\n- two\n\n```rust\nlet x = {n};\n```"), "reasoning": null, "tool_call_id": null, "tool_calls": [] }));
    }
    let dir = home.path().join(".flashagent").join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let session = serde_json::json!({ "id": "session_1_1", "timestamp": 1, "model": "m", "cwd": "~/work", "messages": messages });
    std::fs::write(dir.join("session_1_1.json"), session.to_string()).unwrap();
    let started = std::time::Instant::now();
    let mut term = Term::start(&home, &["-y", "--resume", "session_1_1"], COLS, ROWS);
    term.wait_for(&format!("Answer {}", turns - 1), Duration::from_secs(120));
    println!("resume of {turns} turns to the last answer on screen: {:.0} ms", started.elapsed().as_secs_f64() * 1000.0);
    std::thread::sleep(Duration::from_millis(1500));
    if let Some(mb) = term.pid().and_then(rss_mb) {
        println!("memory with {turns} turns loaded: {mb:.1} MB");
    }
    quit(&mut term);
}

#[test]
fn a_paste_that_arrives_as_keystrokes_is_still_one_prompt() {
    // Without bracketed paste each newline is an Enter; the first line would go
    // to the model alone.
    let server = MockServer::start(vec![Reply::Text("One prompt.".into())]);
    let home = Home::new();
    let term = ready(&home, &server);
    term.write("fn main() {\r    run();\r}");
    term.wait_for("run();", WAIT);
    std::thread::sleep(Duration::from_millis(200));
    assert!(server.turns().is_empty(), "part of the paste was sent on its own");
    term.send(ENTER);
    term.wait_for("One prompt.", WAIT);
    let prompt = prompt_of(server.turns().last().unwrap());
    assert!(prompt.contains("fn main() {\\n    run();\\n}"), "{prompt}");
}

#[test]
fn a_chosen_voice_reaches_the_model_as_an_example_and_nowhere_else() {
    let server = MockServer::start(vec![Reply::Text("Sure.".into())]);
    let home = Home::new();
    let mut term = ready_with(&home, &server, serde_json::json!({ "personality": { "base": "quirky" } }));
    ask(&term, "hello there", "Sure.");

    let body = &server.turns()[0].body;
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "system");
    let system = messages[0]["content"].to_string();
    assert!(system.contains("playful streak"), "the voice is not in the system prompt");
    assert!(!system.contains("user's choice"), "the voice is described as a setting: {system}");
    assert!(messages[2]["content"].to_string().contains("coat check"), "no example in the chosen voice: {messages:?}");
    assert!(messages.last().unwrap()["content"].to_string().contains("hello there"));

    assert!(!term.screen().contains("git stash"), "the example was shown:\n{}", term.screen());
    quit(&mut term);
    let saved: String = home.sessions().iter().map(|p| std::fs::read_to_string(p).unwrap()).collect();
    assert!(!saved.contains("git stash"), "the example was saved with the session");
}

#[test]
fn the_opening_of_the_first_request_is_sent_ahead_so_the_server_has_it_cached() {
    let server = MockServer::start(vec![Reply::Text("Hi there!".into())]);
    let home = Home::new();
    let global = home.path().join(".flashagent");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::write(global.join("MEMORY.md"), "Writes Rust.\n").unwrap();
    let term = ready(&home, &server);

    let deadline = std::time::Instant::now() + WAIT;
    while !server.requests().iter().any(|r| r.is_warm_up()) {
        assert!(std::time::Instant::now() < deadline, "no warm-up was sent before the first message");
        std::thread::sleep(Duration::from_millis(50));
    }
    term.type_text("hello");
    term.send(ENTER);
    term.wait_for("Hi there!", WAIT);

    let warm_ups: Vec<_> = server.requests().into_iter().filter(|r| r.is_warm_up()).collect();
    let turn = &server.turns()[0];
    let warm = warm_ups[0].body["messages"].as_array().unwrap();
    let real = turn.body["messages"].as_array().unwrap();
    assert_eq!(warm.len(), real.len(), "warm-up {warm:?}\nturn {real:?}");
    assert_eq!(warm[..warm.len() - 1], real[..real.len() - 1], "the warm-up began differently from the turn");
    let opening = warm.last().unwrap()["content"].as_str().unwrap();
    let prompt = real.last().unwrap()["content"].as_str().unwrap();
    assert!(opening.contains("Writes Rust"), "the memory block was not warmed: {opening}");
    assert!(prompt.starts_with(opening), "the first prompt does not begin with what was warmed:\n{opening}\n---\n{prompt}");
    assert_eq!(warm_ups[0].body["tools"], turn.body["tools"], "the warm-up offered other tools");
    assert_eq!(warm_ups.len(), 1, "the same prefix was warmed twice");
}


/// The screen is never seen half drawn. The real binary runs in a
/// pseudo-terminal (on Windows, the console's own ConPTY, as under Windows
/// Terminal), and the screen is looked at after every read of its output: a
/// frame that clears first and draws after shows up as a screen suddenly
/// missing most of what it had.
mod never_half_drawn {
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use portable_pty::{native_pty_system, CommandBuilder, PtySize};
    use super::support::mock_server::{MockServer, Reply};
    use super::support::term::Home;

    const COLS: u16 = 120;
    const ROWS: u16 = 40;

    #[derive(Default, Clone, Copy, Debug)]
    struct Seen {
        reads: usize,
        /// Reads after which under 60% of the most the screen ever showed was left.
        half_drawn: usize,
        most: usize,
    }

    struct Watched {
        seen: Arc<Mutex<Seen>>,
        writer: Arc<Mutex<Box<dyn Write + Send>>>,
        child: Box<dyn portable_pty::Child + Send + Sync>,
        _master: Box<dyn portable_pty::MasterPty + Send>,
    }

    impl Watched {
        /// Counting starts once the prompt is up and the welcome card has drawn itself in.
        fn start(home: &Home) -> Self {
            let pty = native_pty_system().openpty(PtySize { rows: ROWS, cols: COLS, pixel_width: 0, pixel_height: 0 }).unwrap();
            let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_flashagent"));
            cmd.arg("-y");
            cmd.cwd(home.work());
            cmd.env("HOME", home.path());
            cmd.env("USERPROFILE", home.path());
            cmd.env("FLASHAGENT_CONFIG_PATH", home.config_path());
            cmd.env("TERM", "xterm-256color");
            let child = pty.slave.spawn_command(cmd).unwrap();
            drop(pty.slave);
            let writer = Arc::new(Mutex::new(pty.master.take_writer().unwrap()));
            let mut reader = pty.master.try_clone_reader().unwrap();
            let seen = Arc::new(Mutex::new(Seen::default()));
            let settled_at = Arc::new(Mutex::new(None::<Instant>));
            let (seen2, writer2, settled2) = (seen.clone(), writer.clone(), settled_at.clone());
            std::thread::spawn(move || {
                let mut parser = vt100::Parser::new(ROWS, COLS, 0);
                let mut buf = [0u8; 65536];
                loop {
                    let n = match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    parser.process(&buf[..n]);
                    if buf[..n].windows(4).any(|w| w == b"\x1b[6n") {
                        let (row, col) = parser.screen().cursor_position();
                        let _ = writer2.lock().unwrap().write_all(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
                    }
                    let contents = parser.screen().contents();
                    let filled = contents.chars().filter(|c| !c.is_whitespace()).count();
                    let mut settled = settled2.lock().unwrap();
                    if settled.is_none() && contents.contains("Ask FlashAgent") {
                        *settled = Some(Instant::now());
                    }
                    let mut seen = seen2.lock().unwrap();
                    if settled.is_some_and(|t| t.elapsed() > Duration::from_secs(1)) {
                        seen.reads += 1;
                        if filled * 10 < seen.most * 6 {
                            seen.half_drawn += 1;
                        }
                    }
                    seen.most = seen.most.max(filled);
                }
            });
            let started = Instant::now();
            while !settled_at.lock().unwrap().is_some_and(|t| t.elapsed() > Duration::from_secs(1)) {
                assert!(started.elapsed() < Duration::from_secs(30), "the prompt never came up");
                std::thread::sleep(Duration::from_millis(50));
            }
            Watched { seen, writer, child, _master: pty.master }
        }

        fn send(&self, keys: &[u8]) {
            self.writer.lock().unwrap().write_all(keys).unwrap();
            std::thread::sleep(Duration::from_millis(120));
        }

        fn seen(&self) -> Seen {
            *self.seen.lock().unwrap()
        }
    }

    impl Drop for Watched {
        fn drop(&mut self) {
            let _ = self.child.kill();
        }
    }

    fn animated(home: &Home, server: &MockServer) {
        home.set_up_with(&server.url, serde_json::json!({ "animations": true, "show_tips": true, "show_mascot": true }));
    }

    #[test]
    fn the_welcome_screen_is_never_seen_half_drawn() {
        let server = MockServer::start(Vec::new());
        let home = Home::new();
        animated(&home, &server);
        let term = Watched::start(&home);

        // The mascot breathes, the tip types itself; then a menu opens and closes, and
        // the prompt is typed into and emptied again.
        std::thread::sleep(Duration::from_secs(3));
        term.send(b"\t");
        std::thread::sleep(Duration::from_millis(800));
        term.send(b"\x1b");
        std::thread::sleep(Duration::from_millis(800));
        for _ in 0..2 {
            for key in [b"a", b"b", b"c"] {
                term.send(key);
            }
            for _ in 0..3 {
                term.send(b"\x7f");
            }
        }
        let seen = term.seen();
        assert!(seen.reads > 20, "too little was drawn to judge: {seen:?}");
        assert_eq!(seen.half_drawn, 0, "the screen was seen half drawn: {seen:?}");
    }

    #[test]
    fn a_streaming_answer_is_never_seen_half_drawn() {
        // Long enough to scroll the welcome card off the screen.
        let story: String = (0..300).map(|i| if i % 12 == 11 { format!("word{i}.\n\n") } else { format!("word{i} ") }).collect();
        let server = MockServer::start(vec![Reply::Slow { text: story, per_word: Duration::from_millis(15) }]);
        let home = Home::new();
        animated(&home, &server);
        let term = Watched::start(&home);

        term.send(b"tell me a story");
        term.send(b"\r");
        std::thread::sleep(Duration::from_secs(6));
        let seen = term.seen();
        assert!(seen.reads > 50, "too little was drawn to judge: {seen:?}");
        assert_eq!(seen.half_drawn, 0, "the screen was seen half drawn: {seen:?}");
    }
}
