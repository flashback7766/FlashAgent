//! How a turn reads in the transcript.
//!
//! The chat is one column of text: a question, the thinking and tool calls
//! done for it, and the answer. Without anything between them they run
//! together and the eye has nothing to hold on to. This file builds a
//! transcript like a real one and checks that the three parts are still
//! three parts — and, with `--ignored --nocapture`, prints it to look at:
//!
//! ```bash
//! cargo test -p flashagent-tui --test chat_demo -- --ignored --nocapture
//! ```

use flashagent_core::loop_::LoopEvent;
use flashagent_tui::{strip_ansi, ChatView, LineKind, ReasoningExpansion};

fn tool(chat: &mut ChatView, name: &str, args: serde_json::Value, result: &str) {
    chat.on_event(&LoopEvent::ToolStarted {
        id: "t".into(),
        name: name.into(),
        args_json: args.to_string(),
    });
    chat.on_event(&LoopEvent::ToolFinished {
        id: "t".into(),
        is_error: false,
        result_len: result.len(),
        result: Some(result.into()),
    });
}

/// Two turns, each with thinking, tool calls and an answer.
fn transcript() -> ChatView {
    let mut chat = ChatView::default();
    chat.push_user("Check out the FlashAgent folder!");
    chat.on_event(&LoopEvent::ReasoningDelta("The user wants an overview of a folder.".into()));
    tool(
        &mut chat,
        "list_dir",
        serde_json::json!({ "header": "Looking for the FlashAgent folder", "path": "." }),
        "FlashAgent/\nnotes.txt",
    );
    chat.on_event(&LoopEvent::TurnDelta("Found it — taking a look inside.\n".into()));
    chat.on_event(&LoopEvent::Done(flashagent_core::DoneReason::Completed));
    tool(
        &mut chat,
        "read_file",
        serde_json::json!({
            "header": "Reading the README and architecture overview",
            "files": [{ "path": "README.md" }, { "path": "ARCHITECTURE.md" }]
        }),
        "# FlashAgent",
    );
    tool(
        &mut chat,
        "run_shell",
        serde_json::json!({ "header": "Checking recent git history", "command": "git log --oneline -5" }),
        "ecf969f Fit the app to a small terminal",
    );
    chat.on_event(&LoopEvent::TurnDelta(
        "Nice project! Here is a quick picture of what is in there:\n\n\
         **FlashAgent** — a local-first AI coding agent in Rust, one 16 MB binary, beta b300.\n\n\
         - The agent loop and permission gating live in core.\n\
         - Tool-call recovery parsing helps small local models.\n"
            .into(),
    ));
    chat.on_event(&LoopEvent::Done(flashagent_core::DoneReason::Completed));

    chat.push_user("And what about the tests?");
    chat.on_event(&LoopEvent::ReasoningDelta("They want the test story now.".into()));
    tool(&mut chat, "grep", serde_json::json!({ "header": "Counting the tests", "pattern": "#\\[test\\]" }), "many");
    chat.on_event(&LoopEvent::TurnDelta(
        "There are 707 of them, and 59 run the real binary in a terminal.".into(),
    ));
    chat.on_event(&LoopEvent::Done(flashagent_core::DoneReason::Completed));
    chat
}

fn drawn(chat: &ChatView, width: usize) -> Vec<(LineKind, String)> {
    let (settled, live) = chat.render_split(width, ReasoningExpansion::none());
    settled.iter().cloned().chain(live).map(|(k, t)| (k, strip_ansi(&t))).collect()
}

#[test]
fn a_question_its_work_and_its_answer_are_three_things_on_screen() {
    let rows = drawn(&transcript(), 100);
    let blank = |i: usize| rows.get(i).is_some_and(|(_, t)| t.trim().is_empty());

    for (i, (kind, _)) in rows.iter().enumerate() {
        let Some((previous, _)) = i.checked_sub(1).and_then(|p| rows.get(p)) else { continue };
        let crossing = matches!(
            (previous, kind),
            (LineKind::User, LineKind::Reasoning | LineKind::Tool)
                | (LineKind::Tool | LineKind::Reasoning, LineKind::Assistant)
                | (LineKind::Assistant, LineKind::Tool | LineKind::Reasoning | LineKind::User)
        );
        assert!(
            !crossing,
            "row {i} moves from one part of the turn to the next with nothing between them:\n{}",
            rows.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join("\n")
        );
    }

    // And the blanks are real: the first one follows the question.
    let question = rows.iter().position(|(k, _)| *k == LineKind::User).expect("the question");
    assert!(blank(question + 1), "nothing separates the question from the work under it");
}

#[test]
fn thinking_and_tool_calls_do_not_look_the_same() {
    let rows = drawn(&transcript(), 100);
    let thought = rows.iter().find(|(k, _)| *k == LineKind::Reasoning).expect("a thought").1.clone();
    let call = rows.iter().find(|(k, _)| *k == LineKind::Tool).expect("a tool call").1.clone();
    assert!(thought.trim_start().starts_with('✻'), "a thought is not marked as one: {thought:?}");
    assert!(call.trim_start().starts_with('▸'), "a tool call is not marked as one: {call:?}");
}

#[test]
fn the_answer_is_the_only_thing_at_the_left_edge() {
    // The model's words are what the transcript is for; the work done for
    // them sits in from the edge, so a glance finds the answers.
    for (kind, text) in drawn(&transcript(), 100) {
        if text.trim().is_empty() {
            continue;
        }
        match kind {
            LineKind::Assistant | LineKind::User => {}
            LineKind::Reasoning | LineKind::Tool | LineKind::ToolError => {
                assert!(text.starts_with("  "), "{kind:?} should be indented: {text:?}");
            }
            _ => {}
        }
    }
}

#[test]
#[ignore = "prints the transcript for a person to look at"]
fn show_chat() {
    for (kind, text) in drawn(&transcript(), 100) {
        let mark = match kind {
            LineKind::User => "U",
            LineKind::Assistant => "A",
            LineKind::Reasoning => "R",
            LineKind::Tool => "T",
            LineKind::ToolError => "E",
            LineKind::Diff => "D",
            LineKind::System => " ",
        };
        println!("{mark}|{text}");
    }
}

#[test]
fn a_transcript_drawn_frame_by_frame_matches_one_drawn_at_once() {
    // Frames reuse what has settled and read only the turn still open. That
    // must never change what is drawn: build the same conversation twice,
    // draw one of them after every event and the other only at the end.
    fn build(draw_each_step: bool) -> Vec<(LineKind, String)> {
        let mut chat = ChatView::default();
        let draw = |chat: &ChatView| {
            if draw_each_step {
                let _ = chat.render_split(100, ReasoningExpansion::none());
            }
        };
        for n in 0..5 {
            chat.push_user(&format!("question {n}"));
            draw(&chat);
            chat.on_event(&LoopEvent::ReasoningDelta(format!("thinking about {n}")));
            draw(&chat);
            tool(&mut chat, "grep", serde_json::json!({ "header": "Searching", "pattern": "x" }), "found");
            draw(&chat);
            chat.on_event(&LoopEvent::TurnDelta(format!("answer {n}\n\n- a point\n")));
            draw(&chat);
            chat.on_event(&LoopEvent::Done(flashagent_core::DoneReason::Completed));
            draw(&chat);
        }
        drawn(&chat, 100)
    }
    assert_eq!(build(true), build(false));
}
