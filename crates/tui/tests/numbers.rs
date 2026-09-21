//! Measurements behind docs/numbers.md. Ignored: they time things, and a
//! busy CI machine would make them flaky. Run them on purpose, in release:
//!
//! ```bash
//! cargo test --release -p flashagent-tui --test numbers -- --ignored --nocapture --test-threads=1
//! ```

use flashagent_core::loop_::LoopEvent;
use flashagent_tui::{ChatView, ReasoningExpansion};
use std::time::Instant;

/// A conversation of `turns` turns: a question, a thought, a tool call and
/// a few lines of answer each — the shape of a real session.
fn session(turns: usize) -> ChatView {
    let mut chat = ChatView::default();
    for n in 0..turns {
        chat.push_user(&format!("Question number {n}: what does this function do?"));
        chat.on_event(&LoopEvent::ReasoningDelta("Reading the file first, then answering.".into()));
        chat.on_event(&LoopEvent::ToolStarted {
            id: format!("t{n}"),
            name: "read_file".into(),
            args_json: r#"{"header":"Reading the file","path":"src/main.rs"}"#.into(),
        });
        chat.on_event(&LoopEvent::ToolFinished { id: format!("t{n}"), is_error: false, result_len: 900, result: Some("fn main() {}".into()) });
        chat.on_event(&LoopEvent::TurnDelta(
            "It parses the arguments, **opens the config** and starts the loop:\n\n- one\n- two\n\n```rust\nfn main() {}\n```\n".into(),
        ));
        chat.on_event(&LoopEvent::Done(flashagent_core::DoneReason::Completed));
    }
    chat
}

fn time<T>(what: &str, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let out = f();
    println!("{what}: {:.1} ms", start.elapsed().as_secs_f64() * 1000.0);
    out
}

#[test]
#[ignore = "a measurement, not a check"]
fn a_frame_of_a_long_session() {
    for turns in [100, 1_000, 10_000] {
        let chat = session(turns);
        // The first frame renders everything; later frames reuse what has
        // settled, which is what a user feels while the model streams.
        time(&format!("{turns} turns, first frame"), || chat.render_split(120, ReasoningExpansion::none()));
        let mut chat = chat;
        chat.push_user("one more");
        chat.on_event(&LoopEvent::TurnDelta("streaming".into()));
        let frames = 20;
        let start = Instant::now();
        for i in 0..frames {
            chat.on_event(&LoopEvent::TurnDelta(format!(" word{i}")));
            let _ = chat.render_split(120, ReasoningExpansion::none());
        }
        println!("{turns} turns, streaming frame: {:.2} ms", start.elapsed().as_secs_f64() * 1000.0 / frames as f64);
    }
}
