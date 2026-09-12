//! Does this model actually drive tools?
//!
//! Local models vary wildly here, and the failure is quiet: the agent looks
//! like it is working while the model narrates what it would do. These
//! scenarios are the smallest set that separates a model you can hand a task
//! to from one you cannot — each maps to something the agent loop does on
//! every turn, so a failure here is a failure you will meet within minutes.

use std::time::{Duration, Instant};

use flashagent_llm::{ChatMessage, LlmEvent, ScannerEvent, TextToolScanner, ToolSpec, TurnOptions};
use futures::StreamExt;

use crate::LlmSource;

/// How the model expressed a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallStyle {
    /// The backend reported a real `tool_calls` delta.
    Native,
    /// The call arrived as text (`<tool_call>`, `[TOOL_CALLS]`, bare JSON) and
    /// FlashAgent's scanner recovered it.
    Recovered,
}

impl CallStyle {
    /// Word for a table cell.
    pub fn label(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Recovered => "recovered",
        }
    }
}

/// What one scenario did.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The model did exactly the right thing.
    Pass { style: CallStyle, secs: f32 },
    /// It called the right tool but got the arguments wrong — the loop runs,
    /// the work is wrong.
    Partial { detail: String, secs: f32 },
    /// It did not do the thing at all.
    Fail { detail: String, secs: f32 },
}

impl Outcome {
    /// Whether this counts towards the score.
    pub fn is_pass(&self) -> bool {
        matches!(self, Self::Pass { .. })
    }

    fn secs(&self) -> f32 {
        match self {
            Self::Pass { secs, .. } | Self::Partial { secs, .. } | Self::Fail { secs, .. } => *secs,
        }
    }

    fn detail(&self) -> String {
        match self {
            Self::Pass { style, secs } => format!("{}, {secs:.1}s", style.label()),
            Self::Partial { detail, .. } | Self::Fail { detail, .. } => detail.clone(),
        }
    }
}

/// One check, and what the model is expected to do.
pub struct Scenario {
    /// Stable identifier, used in machine-readable output.
    pub key: &'static str,
    /// One line describing what is being checked.
    pub title: &'static str,
    /// Why a failure here matters.
    pub matters: &'static str,
}

/// Every scenario, in the order they are run.
pub fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            key: "single_call",
            title: "makes a tool call at all",
            matters: "without this the agent can only talk about doing the work",
        },
        Scenario {
            key: "arguments",
            title: "gets the arguments right",
            matters: "wrong arguments edit the wrong file or run the wrong command",
        },
        Scenario {
            key: "no_spurious_call",
            title: "answers plainly when no tool is needed",
            matters: "a model that always calls something loops instead of replying",
        },
        Scenario {
            key: "uses_result",
            title: "uses a tool result instead of calling again",
            matters: "otherwise every turn repeats the same call until the budget ends it",
        },
        Scenario {
            key: "second_step",
            title: "moves on to the next tool",
            matters: "multi-step work is the whole point of an agent",
        },
        Scenario {
            key: "exact_content",
            title: "keeps content exact through JSON",
            matters: "quotes and newlines mangled in transit corrupt the file being written",
        },
        Scenario {
            key: "after_error",
            title: "recovers from a failed tool call",
            matters: "tools fail constantly; a model that cannot adapt stalls on the first one",
        },
        Scenario {
            key: "two_calls",
            title: "asks for two files in one turn",
            matters: "one call per turn turns a ten-file job into ten round trips",
        },
    ]
}

fn probe_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "read_lines".into(),
            description: "Read the first N lines of a file.".into(),
            parameters_json: r#"{"type":"object","properties":{"path":{"type":"string"},"count":{"type":"integer"}},"required":["path","count"]}"#.into(),
        },
        ToolSpec {
            name: "write_file".into(),
            description: "Write content to a file, replacing it.".into(),
            parameters_json: r#"{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}"#.into(),
        },
        ToolSpec {
            name: "report_status".into(),
            description: "Report a numeric status code back to the harness.".into(),
            parameters_json: r#"{"type":"object","properties":{"code":{"type":"integer"}},"required":["code"]}"#.into(),
        },
    ]
}

fn opts() -> TurnOptions {
    TurnOptions {
        thinking: flashagent_llm::ThinkingEffort::Off,
        temperature: Some(0.0),
        max_tokens: Some(1024),
        ..Default::default()
    }
}

/// What one turn produced. Calls are kept in full: a turn may legitimately
/// carry several, and whether it does is itself one of the things measured.
struct Turn {
    calls: Vec<(String, String, CallStyle)>,
    text: String,
    secs: f32,
    error: Option<String>,
}

impl Turn {
    fn first_call(&self) -> Option<&(String, String, CallStyle)> {
        self.calls.first()
    }
}

async fn run_turn(llm: &dyn LlmSource, messages: &[ChatMessage], timeout: Duration) -> Turn {
    let started = Instant::now();
    let tools = probe_tools();
    let collect = async {
        let mut stream = llm
            .turn_with_options(messages, &tools, &opts())
            .await
            .map_err(|e| e.to_string())?;
        // Deltas arrive interleaved by index, the same way the agent loop
        // assembles them.
        let mut parts: Vec<(String, String)> = Vec::new();
        let mut text = String::new();
        while let Some(ev) = stream.next().await {
            match ev.map_err(|e| e.to_string())? {
                LlmEvent::ToolCallDelta { index, name, args_delta, .. } => {
                    if parts.len() <= index {
                        parts.resize(index + 1, (String::new(), String::new()));
                    }
                    let slot = &mut parts[index];
                    if let Some(n) = name {
                        if slot.0.is_empty() {
                            slot.0 = n;
                        }
                    }
                    slot.1.push_str(&args_delta);
                }
                LlmEvent::TextDelta(t) => text.push_str(&t),
                _ => {}
            }
        }
        Ok::<_, String>((parts, text))
    };

    match tokio::time::timeout(timeout, collect).await {
        Err(_) => Turn {
            calls: Vec::new(),
            text: String::new(),
            secs: started.elapsed().as_secs_f32(),
            error: Some(format!("no answer within {}s", timeout.as_secs())),
        },
        Ok(Err(e)) => Turn {
            calls: Vec::new(),
            text: String::new(),
            secs: started.elapsed().as_secs_f32(),
            error: Some(e),
        },
        Ok(Ok((parts, text))) => {
            let secs = started.elapsed().as_secs_f32();
            let mut calls: Vec<(String, String, CallStyle)> = parts
                .into_iter()
                .filter(|(name, _)| !name.is_empty())
                .map(|(name, args)| (name, args, CallStyle::Native))
                .collect();
            if calls.is_empty() {
                // The same recovery the agent loop uses, so the score reflects
                // what a real run would do, not what the backend reported.
                let mut scanner = TextToolScanner::default();
                let mut events = scanner.feed(&text);
                events.extend(scanner.finish());
                calls = events
                    .into_iter()
                    .filter_map(|e| match e {
                        ScannerEvent::ToolCall { name, args_json, .. } => {
                            Some((name, args_json, CallStyle::Recovered))
                        }
                        ScannerEvent::Text(_) => None,
                    })
                    .collect();
            }
            Turn { calls, text, secs, error: None }
        }
    }
}

fn parsed_args(raw: &str) -> Option<serde_json::Value> {
    flashagent_llm::effective_args(raw, "")
        .or_else(|| serde_json::from_str(raw).ok())
        .or_else(|| flashagent_llm::repair_json(raw).and_then(|r| serde_json::from_str(&r).ok()))
}

fn snippet(text: &str) -> String {
    let t: String = text.trim().chars().take(70).collect();
    if t.is_empty() {
        "(no output)".to_string()
    } else {
        t
    }
}

/// The result of checking one model.
#[derive(Debug, Clone)]
pub struct CheckReport {
    /// Model id as the server names it.
    pub model: String,
    /// Outcome per scenario, in [`scenarios`] order.
    pub results: Vec<(String, Outcome)>,
}

impl CheckReport {
    /// Passed scenarios out of the total.
    pub fn score(&self) -> (usize, usize) {
        (self.results.iter().filter(|(_, o)| o.is_pass()).count(), self.results.len())
    }

    /// Whether any call arrived only because the text scanner rescued it —
    /// worth saying out loud, since it is slower and more fragile.
    pub fn needed_recovery(&self) -> bool {
        self.results
            .iter()
            .any(|(_, o)| matches!(o, Outcome::Pass { style: CallStyle::Recovered, .. }))
    }

    /// Total seconds spent waiting on the model.
    pub fn total_secs(&self) -> f32 {
        self.results.iter().map(|(_, o)| o.secs()).sum()
    }

    /// One-line judgement, in plain words.
    pub fn verdict(&self) -> &'static str {
        let (passed, total) = self.score();
        match (passed, total) {
            (p, t) if p == t => "drives tools reliably",
            (p, t) if p + 1 == t => "usable, with one rough edge",
            (p, t) if p * 2 >= t => "unreliable — expect to babysit it",
            (0, _) => "cannot drive tools",
            _ => "mostly fails to drive tools",
        }
    }

    /// Human-readable lines for a terminal.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        let title_width = scenarios().iter().map(|s| s.title.len()).max().unwrap_or(40);
        for (scenario, (key, outcome)) in scenarios().iter().zip(&self.results) {
            debug_assert_eq!(scenario.key, key);
            let mark = if outcome.is_pass() { "ok  " } else { "FAIL" };
            out.push(format!("  {mark}  {:<width$} {}", scenario.title, outcome.detail(), width = title_width));
        }
        let (passed, total) = self.score();
        out.push(format!(
            "  {passed}/{total} — {}{}",
            self.verdict(),
            if self.needed_recovery() { " (calls recovered from text, not native)" } else { "" }
        ));
        out
    }

    /// A row for the README table.
    pub fn markdown_row(&self) -> String {
        let (passed, total) = self.score();
        let style = if self.needed_recovery() { "text, recovered" } else { "native" };
        format!(
            "| `{}` | {passed}/{total} | {style} | {:.0}s | {} |",
            self.model,
            self.total_secs(),
            self.verdict()
        )
    }

    /// Machine-readable form, so a published table can be checked.
    pub fn to_json(&self) -> serde_json::Value {
        let (passed, total) = self.score();
        serde_json::json!({
            "model": self.model,
            "passed": passed,
            "total": total,
            "verdict": self.verdict(),
            "needed_recovery": self.needed_recovery(),
            "seconds": self.total_secs(),
            "scenarios": self.results.iter().map(|(key, outcome)| serde_json::json!({
                "key": key,
                "pass": outcome.is_pass(),
                "detail": outcome.detail(),
                "seconds": outcome.secs(),
            })).collect::<Vec<_>>(),
        })
    }
}

/// Header for the README table these rows go into.
pub const MARKDOWN_HEADER: &str =
    "| Model | Score | Tool calls | Time | Verdict |\n| :--- | :--- | :--- | :--- | :--- |";

/// Models a chat check makes no sense for: they never answer a chat request,
/// so scoring them says nothing about the ones that do.
pub fn is_chat_model(id: &str) -> bool {
    let lower = id.to_lowercase();
    !(lower.contains("embed") || lower.contains("rerank") || lower.contains("whisper"))
}

/// Run every scenario against `llm`. `timeout` caps each turn, not the run.
pub async fn check_model(llm: &dyn LlmSource, model: &str, timeout: Duration) -> CheckReport {
    let mut results = Vec::new();

    // 1. A tool call at all.
    let turn = run_turn(
        llm,
        &[
            ChatMessage::system("You are a tool-calling test harness. Answer only by calling a tool."),
            ChatMessage::user("Call report_status with code 42."),
        ],
        timeout,
    )
    .await;
    results.push(("single_call".to_string(), judge_call(&turn, "report_status", |args| {
        match args.get("code").and_then(|c| c.as_i64()) {
            Some(42) => Ok(()),
            other => Err(format!("code was {other:?}, expected 42")),
        }
    })));

    // 2. Two arguments of different types, both of which must survive.
    let turn = run_turn(
        llm,
        &[
            ChatMessage::system("You are a coding agent. Use the tools to do what is asked."),
            ChatMessage::user("Show me the first 20 lines of src/main.rs."),
        ],
        timeout,
    )
    .await;
    results.push(("arguments".to_string(), judge_call(&turn, "read_lines", |args| {
        let path = args.get("path").and_then(|p| p.as_str()).unwrap_or_default();
        let count = args.get("count").and_then(|c| c.as_i64());
        match (path.ends_with("src/main.rs"), count) {
            (true, Some(20)) => Ok(()),
            (false, _) => Err(format!("path was {path:?}, expected src/main.rs")),
            (_, other) => Err(format!("count was {other:?}, expected 20")),
        }
    })));

    // 3. A question with no tool in it. Advertising tools must not turn every
    //    reply into a call.
    let turn = run_turn(
        llm,
        &[
            ChatMessage::system("You are a coding agent. Use the tools only when they are needed."),
            ChatMessage::user("In one sentence: what does a compiler do?"),
        ],
        timeout,
    )
    .await;
    results.push((
        "no_spurious_call".to_string(),
        match (&turn.error, turn.first_call()) {
            (Some(e), _) => Outcome::Fail { detail: e.clone(), secs: turn.secs },
            (None, Some((name, _, _))) => Outcome::Fail {
                detail: format!("called {name} on a question that needed no tool"),
                secs: turn.secs,
            },
            (None, None) if turn.text.trim().is_empty() => {
                Outcome::Fail { detail: "empty reply".to_string(), secs: turn.secs }
            }
            (None, None) => Outcome::Pass { style: CallStyle::Native, secs: turn.secs },
        },
    ));

    // 4. A tool result is already in the history; the answer is in it.
    let mut asked = ChatMessage::assistant("");
    asked.tool_calls = vec![flashagent_llm::ToolCall {
        id: "call_1".into(),
        name: "read_lines".into(),
        args_json: r#"{"path":"version.txt","count":1}"#.into(),
    }];
    let turn = run_turn(
        llm,
        &[
            ChatMessage::system("You are a coding agent. Use the tools to do what is asked."),
            ChatMessage::user("What version is in version.txt?"),
            asked,
            ChatMessage::tool_result("call_1", "version = v4.2.1"),
        ],
        timeout,
    )
    .await;
    results.push((
        "uses_result".to_string(),
        match (&turn.error, turn.first_call()) {
            (Some(e), _) => Outcome::Fail { detail: e.clone(), secs: turn.secs },
            (None, Some((name, _, _))) => Outcome::Fail {
                detail: format!("called {name} again instead of answering from the result"),
                secs: turn.secs,
            },
            (None, None) if turn.text.contains("4.2.1") => {
                Outcome::Pass { style: CallStyle::Native, secs: turn.secs }
            }
            (None, None) => Outcome::Partial {
                detail: format!("answered without the version: {}", snippet(&turn.text)),
                secs: turn.secs,
            },
        },
    ));

    // 5. One tool done, the next one due.
    let mut first = ChatMessage::assistant("");
    first.tool_calls = vec![flashagent_llm::ToolCall {
        id: "call_2".into(),
        name: "read_lines".into(),
        args_json: r#"{"path":"status.txt","count":1}"#.into(),
    }];
    let turn = run_turn(
        llm,
        &[
            ChatMessage::system("You are a coding agent. Use the tools to do what is asked."),
            ChatMessage::user(
                "Read status.txt, then report the number it contains with report_status.",
            ),
            first,
            ChatMessage::tool_result("call_2", "status = 7"),
        ],
        timeout,
    )
    .await;
    results.push(("second_step".to_string(), judge_call(&turn, "report_status", |args| {
        match args.get("code").and_then(|c| c.as_i64()) {
            Some(7) => Ok(()),
            other => Err(format!("code was {other:?}, expected 7 from the file")),
        }
    })));

    // 6. Content with the characters that break naive JSON encoding.
    const EXACT: &str = "line one\n\"quoted\"\nend";
    let turn = run_turn(
        llm,
        &[
            ChatMessage::system("You are a coding agent. Use the tools to do what is asked."),
            ChatMessage::user(
                "Write exactly these three lines to notes.txt, nothing else:\nline one\n\"quoted\"\nend",
            ),
        ],
        timeout,
    )
    .await;
    results.push(("exact_content".to_string(), judge_call(&turn, "write_file", |args| {
        let content = args.get("content").and_then(|c| c.as_str()).unwrap_or_default();
        let normalised = content.replace("\r\n", "\n");
        let trimmed = normalised.trim_end_matches('\n');
        if trimmed == EXACT {
            Ok(())
        } else {
            Err(format!("content came through as {trimmed:?}"))
        }
    })));

    // 7. The first attempt failed. Adapt, do not repeat and do not give up.
    let mut failed = ChatMessage::assistant("");
    failed.tool_calls = vec![flashagent_llm::ToolCall {
        id: "call_3".into(),
        name: "read_lines".into(),
        args_json: r#"{"path":"src/confg.rs","count":5}"#.into(),
    }];
    let turn = run_turn(
        llm,
        &[
            ChatMessage::system("You are a coding agent. Use the tools to do what is asked."),
            ChatMessage::user("Read the first 5 lines of src/config.rs."),
            failed,
            ChatMessage::tool_result(
                "call_3",
                "error: no such file: src/confg.rs (did you mean src/config.rs?)",
            ),
        ],
        timeout,
    )
    .await;
    results.push(("after_error".to_string(), judge_call(&turn, "read_lines", |args| {
        match args.get("path").and_then(|p| p.as_str()) {
            Some(path) if path.ends_with("src/config.rs") => Ok(()),
            Some(path) => Err(format!("retried with {path:?} instead of the corrected path")),
            None => Err("no path in the retry".to_string()),
        }
    })));

    // 8. Two files asked for at once: does the turn carry both calls?
    let turn = run_turn(
        llm,
        &[
            ChatMessage::system(
                "You are a coding agent. Use the tools to do what is asked, in as few turns as possible.",
            ),
            ChatMessage::user("Read the first 10 lines of both Cargo.toml and README.md."),
        ],
        timeout,
    )
    .await;
    results.push((
        "two_calls".to_string(),
        match (&turn.error, turn.calls.len()) {
            (Some(e), _) => Outcome::Fail { detail: e.clone(), secs: turn.secs },
            (None, 0) => Outcome::Fail {
                detail: format!("replied with text, no tool call: {}", snippet(&turn.text)),
                secs: turn.secs,
            },
            (None, 1) => Outcome::Partial {
                detail: "one call for two files — the loop will need a second turn".to_string(),
                secs: turn.secs,
            },
            (None, _) => Outcome::Pass {
                style: turn.calls.first().map(|c| c.2).unwrap_or(CallStyle::Native),
                secs: turn.secs,
            },
        },
    ));

    CheckReport { model: model.to_string(), results }
}

/// Shared judgement for the scenarios that must end in a specific call.
fn judge_call(
    turn: &Turn,
    expected: &str,
    check_args: impl Fn(&serde_json::Value) -> Result<(), String>,
) -> Outcome {
    if let Some(e) = &turn.error {
        return Outcome::Fail { detail: e.clone(), secs: turn.secs };
    }
    let Some((name, args, style)) = turn.first_call() else {
        return Outcome::Fail {
            detail: format!("replied with text, no tool call: {}", snippet(&turn.text)),
            secs: turn.secs,
        };
    };
    if name != expected {
        return Outcome::Fail {
            detail: format!("called {name}, expected {expected}"),
            secs: turn.secs,
        };
    }
    let Some(parsed) = parsed_args(args) else {
        return Outcome::Partial {
            detail: format!("arguments were not JSON: {}", snippet(args)),
            secs: turn.secs,
        };
    };
    match check_args(&parsed) {
        Ok(()) => Outcome::Pass { style: *style, secs: turn.secs },
        Err(detail) => Outcome::Partial { detail, secs: turn.secs },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use flashagent_llm::{FinishReason, LlmError};
    use futures::stream::BoxStream;
    use std::sync::Mutex;

    /// A backend that answers each turn from a script.
    struct Scripted {
        turns: Mutex<Vec<Vec<LlmEvent>>>,
    }

    impl Scripted {
        fn new(turns: Vec<Vec<LlmEvent>>) -> Self {
            Self { turns: Mutex::new(turns) }
        }
    }

    #[async_trait]
    impl LlmSource for Scripted {
        async fn turn(
            &self,
            _messages: &[ChatMessage],
            _tools: &[ToolSpec],
        ) -> Result<BoxStream<'static, Result<LlmEvent, LlmError>>, LlmError> {
            let mut turns = self.turns.lock().unwrap();
            let events = if turns.is_empty() { Vec::new() } else { turns.remove(0) };
            Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
        }
    }

    fn call(name: &str, args: &str) -> Vec<LlmEvent> {
        calls(&[(name, args)])
    }

    /// One turn carrying several calls, the way a model that batches answers.
    fn calls(specs: &[(&str, &str)]) -> Vec<LlmEvent> {
        let mut events: Vec<LlmEvent> = specs
            .iter()
            .enumerate()
            .map(|(index, (name, args))| LlmEvent::ToolCallDelta {
                index,
                id: Some(format!("c{index}")),
                name: Some((*name).into()),
                args_delta: (*args).into(),
            })
            .collect();
        events.push(LlmEvent::Done(FinishReason::ToolUse));
        events
    }

    fn text(body: &str) -> Vec<LlmEvent> {
        vec![LlmEvent::TextDelta(body.into()), LlmEvent::Done(FinishReason::Stop)]
    }

    fn run(turns: Vec<Vec<LlmEvent>>) -> CheckReport {
        let llm = Scripted::new(turns);
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(check_model(&llm, "scripted", Duration::from_secs(5)))
    }

    fn perfect() -> Vec<Vec<LlmEvent>> {
        vec![
            call("report_status", r#"{"code":42}"#),
            call("read_lines", r#"{"path":"src/main.rs","count":20}"#),
            text("It turns source code into machine code."),
            text("version.txt says v4.2.1."),
            call("report_status", r#"{"code":7}"#),
            call("write_file", r#"{"path":"notes.txt","content":"line one\n\"quoted\"\nend"}"#),
            call("read_lines", r#"{"path":"src/config.rs","count":5}"#),
            calls(&[
                ("read_lines", r#"{"path":"Cargo.toml","count":10}"#),
                ("read_lines", r#"{"path":"README.md","count":10}"#),
            ]),
        ]
    }

    #[test]
    fn a_model_that_does_everything_right_scores_full_marks() {
        let report = run(perfect());
        assert_eq!(report.score(), (8, 8), "{:?}", report.results);
        assert_eq!(report.verdict(), "drives tools reliably");
        assert!(!report.needed_recovery());
    }

    #[test]
    fn a_model_that_only_talks_scores_nothing_useful() {
        let report = run(vec![
            text("I would call report_status with code 42."),
            text("Sure, I can read that file for you."),
            text("A compiler translates source code."),
            text("The version is v4.2.1."),
            text("I will now report the status."),
        ]);
        // Only the two scenarios that ask for prose can pass.
        // Two of the five ask for prose, and prose is all it produced.
        assert_eq!(report.score(), (2, 8), "{:?}", report.results);
        assert_eq!(report.verdict(), "mostly fails to drive tools");
    }

    #[test]
    fn calls_written_as_text_count_but_are_flagged() {
        // Plenty of small models emit Hermes markup instead of a tool_calls
        // delta; the loop recovers those, so the score must too — while
        // saying that is what happened.
        let mut turns = perfect();
        turns[0] = text("<tool_call>{\"name\": \"report_status\", \"arguments\": {\"code\": 42}}</tool_call>");
        let report = run(turns);
        assert_eq!(report.score(), (8, 8), "{:?}", report.results);
        assert!(report.needed_recovery(), "recovery must be visible in the report");
        assert!(report.markdown_row().contains("text, recovered"));
    }

    #[test]
    fn wrong_arguments_are_not_a_pass() {
        let mut turns = perfect();
        turns[1] = call("read_lines", r#"{"path":"src/lib.rs","count":20}"#);
        let report = run(turns);
        assert_eq!(report.score(), (7, 8));
        let (_, outcome) = &report.results[1];
        assert!(matches!(outcome, Outcome::Partial { .. }), "{outcome:?}");
        assert!(outcome.detail().contains("src/main.rs"), "{}", outcome.detail());
    }

    #[test]
    fn calling_a_tool_for_a_plain_question_is_a_failure() {
        let mut turns = perfect();
        turns[2] = call("read_lines", r#"{"path":"compiler.md","count":5}"#);
        let report = run(turns);
        assert_eq!(report.score(), (7, 8));
        assert!(report.results[2].1.detail().contains("needed no tool"));
    }

    #[test]
    fn repeating_a_call_instead_of_using_its_result_is_a_failure() {
        let mut turns = perfect();
        turns[3] = call("read_lines", r#"{"path":"version.txt","count":1}"#);
        let report = run(turns);
        assert_eq!(report.score(), (7, 8));
        assert!(report.results[3].1.detail().contains("instead of answering"));
    }

    #[test]
    fn a_backend_that_says_nothing_fails_rather_than_passes() {
        let report = run(vec![Vec::new(); scenarios().len()]);
        assert_eq!(report.score(), (0, 8), "{:?}", report.results);
        assert_eq!(report.verdict(), "cannot drive tools");
    }

    #[test]
    fn content_mangled_in_transit_is_not_a_pass() {
        // Quotes and newlines are where naive JSON encoding breaks, and a
        // model that loses them corrupts the file it is writing.
        let mut turns = perfect();
        turns[5] = call("write_file", r#"{"path":"notes.txt","content":"line one quoted end"}"#);
        let report = run(turns);
        assert_eq!(report.score(), (7, 8));
        assert!(report.results[5].1.detail().contains("came through as"));
    }

    #[test]
    fn repeating_a_failed_call_unchanged_is_not_a_pass() {
        let mut turns = perfect();
        turns[6] = call("read_lines", r#"{"path":"src/confg.rs","count":5}"#);
        let report = run(turns);
        assert_eq!(report.score(), (7, 8));
        assert!(report.results[6].1.detail().contains("instead of the corrected path"));
    }

    #[test]
    fn one_call_for_two_files_is_only_half_the_work() {
        let mut turns = perfect();
        turns[7] = call("read_lines", r#"{"path":"Cargo.toml","count":10}"#);
        let report = run(turns);
        assert_eq!(report.score(), (7, 8));
        assert!(report.results[7].1.detail().contains("second turn"));
    }

    #[test]
    fn the_report_renders_for_humans_and_for_a_table() {
        let report = run(perfect());
        let lines = report.lines();
        assert_eq!(lines.len(), scenarios().len() + 1);
        assert!(lines.last().unwrap().contains("8/8"));
        assert!(report.markdown_row().starts_with("| `scripted` | 8/8 |"));
        assert_eq!(report.to_json()["passed"], 8);
        assert_eq!(report.to_json()["scenarios"].as_array().unwrap().len(), 8);
    }
}
