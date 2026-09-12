//! `/goal` budgets and the end-of-run report.
//!
//! Two jobs, both deliberately dumb and testable: parse the budget flags off
//! the command line, and keep a ledger of what actually happened during the
//! run — built from [`LoopEvent`]s, never from the model's own account of its
//! work. The final card states facts (files touched, commands that failed,
//! why the run stopped) next to the model's summary, so a goal that stopped
//! at a budget cannot read as a goal that finished.

use std::time::{Duration, Instant};

use flashagent_core::{DoneReason, LoopEvent};

/// Step budget used when `/goal` is given no `--steps`.
pub const DEFAULT_STEPS: u32 = 250;
/// Wall-clock budget used when `/goal` is given no `--time`.
pub const DEFAULT_TIME: Duration = Duration::from_secs(60 * 60);

/// Budgets for one `/goal` run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalBudgets {
    /// Loop iterations.
    pub steps: Option<u32>,
    /// Wall clock.
    pub time: Option<Duration>,
    /// Generated (completion) tokens; `None` = unlimited.
    pub output_tokens: Option<i64>,
}

impl Default for GoalBudgets {
    fn default() -> Self {
        Self { steps: Some(DEFAULT_STEPS), time: Some(DEFAULT_TIME), output_tokens: None }
    }
}

impl GoalBudgets {
    /// Budgets for an ordinary chat turn: the session's step cap and nothing
    /// else — time and token budgets exist only for `/goal`.
    pub fn steps_only(steps: Option<u32>) -> Self {
        Self { steps, time: None, output_tokens: None }
    }

    /// One-line summary for the framing card.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        match self.steps {
            Some(n) => parts.push(format!("{n} steps")),
            None => parts.push("unlimited steps".to_string()),
        }
        if let Some(t) = self.time {
            parts.push(format!("{} max", human_duration(t)));
        }
        if let Some(t) = self.output_tokens {
            parts.push(format!("{} generated tokens", human_count(t)));
        }
        parts.join(" · ")
    }
}

/// Parse `[--steps N] [--time 30m] [--tokens 200k] <task>`.
///
/// Flags are only recognised before the task text: `/goal fix --tokens in the
/// parser` keeps `--tokens` as part of the task. Returns the budgets and the
/// task, or a message to show the user.
pub fn parse_goal_command(rest: &str) -> Result<(GoalBudgets, String), String> {
    let mut budgets = GoalBudgets::default();
    let mut words = rest.split_whitespace().peekable();
    let mut consumed = 0usize;

    while let Some(word) = words.peek().copied() {
        if !word.starts_with("--") {
            break;
        }
        let (flag, inline) = match word.split_once('=') {
            Some((f, v)) => (f, Some(v.to_string())),
            None => (word, None),
        };
        let known = matches!(flag, "--steps" | "--time" | "--tokens");
        if !known {
            return Err(format!(
                "Unknown option {flag}. Usage: /goal [--steps N] [--time 30m] [--tokens 200k] <task>"
            ));
        }
        words.next();
        consumed += 1;
        let value = match inline {
            Some(v) => v,
            None => {
                let v = words
                    .next()
                    .ok_or_else(|| format!("{flag} needs a value, e.g. {}", flag_example(flag)))?;
                consumed += 1;
                v.to_string()
            }
        };
        match flag {
            "--steps" => {
                budgets.steps = Some(parse_limit(&value, flag)?.try_into().map_err(|_| {
                    format!("{flag}: {value} is too large")
                })?)
            }
            "--time" => budgets.time = Some(parse_duration(&value)?),
            "--tokens" => budgets.output_tokens = Some(parse_limit(&value, flag)?),
            _ => unreachable!(),
        }
    }

    // Re-derive the task from the original text so its internal spacing and
    // newlines survive: splitting on whitespace would flatten them.
    let task = skip_words(rest, consumed).trim().to_string();
    if task.is_empty() {
        return Err("Nothing to do. Usage: /goal [--steps N] [--time 30m] [--tokens 200k] <task>"
            .to_string());
    }
    Ok((budgets, task))
}

fn flag_example(flag: &str) -> &'static str {
    match flag {
        "--steps" => "--steps 120",
        "--time" => "--time 30m",
        _ => "--tokens 200k",
    }
}

/// Return `text` with the first `n` whitespace-separated words removed,
/// keeping everything after them byte for byte.
fn skip_words(text: &str, n: usize) -> &str {
    let mut rest = text;
    for _ in 0..n {
        rest = rest.trim_start();
        match rest.find(char::is_whitespace) {
            Some(i) => rest = &rest[i..],
            None => return "",
        }
    }
    rest
}

fn parse_limit(value: &str, flag: &str) -> Result<i64, String> {
    let v = value.trim().to_lowercase();
    let (digits, mult) = match v.strip_suffix('k') {
        Some(d) => (d, 1_000i64),
        None => match v.strip_suffix('m') {
            Some(d) => (d, 1_000_000i64),
            None => (v.as_str(), 1),
        },
    };
    let n: f64 = digits
        .parse()
        .map_err(|_| format!("{flag}: expected a number, got {value}"))?;
    if !(n.is_finite() && n > 0.0) {
        return Err(format!("{flag}: expected a positive number, got {value}"));
    }
    Ok((n * mult as f64).round() as i64)
}

fn parse_duration(value: &str) -> Result<Duration, String> {
    let v = value.trim().to_lowercase();
    let (digits, unit_secs) = if let Some(d) = v.strip_suffix('h') {
        (d, 3600.0)
    } else if let Some(d) = v.strip_suffix("min") {
        (d, 60.0)
    } else if let Some(d) = v.strip_suffix('m') {
        (d, 60.0)
    } else if let Some(d) = v.strip_suffix('s') {
        (d, 1.0)
    } else {
        (v.as_str(), 60.0) // a bare number means minutes
    };
    let n: f64 = digits
        .parse()
        .map_err(|_| format!("--time: expected a duration like 30m, 90s or 2h, got {value}"))?;
    if !(n.is_finite() && n > 0.0) {
        return Err(format!("--time: expected a positive duration, got {value}"));
    }
    Ok(Duration::from_secs_f64(n * unit_secs))
}

/// `93s` → `1m33s`, `3600s` → `1h0m`. Under a minute keeps one decimal, so a
/// run that took 800 ms does not report itself as having taken no time.
pub fn human_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

/// `1234` → `1.2k`.
pub fn human_count(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// What a `/goal` run actually did, accumulated from loop events.
pub struct GoalLedger {
    /// The task as the user phrased it.
    pub task: String,
    /// Budgets this run was started with.
    pub budgets: GoalBudgets,
    started: Instant,
    step: u32,
    output_tokens: i64,
    written: Vec<String>,
    edited: Vec<String>,
    shell_ok: usize,
    shell_failed: Vec<String>,
    tool_failures: Vec<String>,
    open_calls: Vec<(String, String, String)>, // id, tool name, subject
}

impl GoalLedger {
    /// Start a ledger for `task`.
    pub fn new(task: String, budgets: GoalBudgets) -> Self {
        Self {
            task,
            budgets,
            started: Instant::now(),
            step: 0,
            output_tokens: 0,
            written: Vec::new(),
            edited: Vec::new(),
            shell_ok: 0,
            shell_failed: Vec::new(),
            tool_failures: Vec::new(),
            open_calls: Vec::new(),
        }
    }

    /// Current step number (0 before the first turn is requested).
    pub fn step(&self) -> u32 {
        self.step
    }

    /// Generated tokens so far.
    pub fn output_tokens(&self) -> i64 {
        self.output_tokens
    }

    /// Time since the goal was launched.
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Feed one loop event.
    pub fn on_event(&mut self, ev: &LoopEvent) {
        match ev {
            LoopEvent::StepStarted { step, .. } => self.step = *step,
            LoopEvent::Usage(u) => self.output_tokens += u.completion.unwrap_or(0),
            LoopEvent::ToolStarted { id, name, args_json } => {
                let subject = subject_of(name, args_json);
                self.open_calls.push((id.clone(), name.clone(), subject));
            }
            LoopEvent::ToolFinished { id, is_error, result, .. } => {
                let Some(pos) = self.open_calls.iter().position(|(cid, _, _)| cid == id) else {
                    return;
                };
                let (_, name, subject) = self.open_calls.remove(pos);
                match (name.as_str(), *is_error) {
                    ("write_file", false) => push_unique(&mut self.written, subject),
                    ("edit_file" | "patch_file", false) => push_unique(&mut self.edited, subject),
                    ("run_shell", false) => self.shell_ok += 1,
                    ("run_shell", true) => {
                        let why = first_line(result.as_deref().unwrap_or(""));
                        self.shell_failed.push(format!("{subject} — {why}"));
                    }
                    (_, true) => {
                        let why = first_line(result.as_deref().unwrap_or(""));
                        self.tool_failures.push(format!("{name}({subject}) — {why}"));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// Live progress for the status line: `step 12/250 · 4.2k tok · 3m05s/1h0m`.
    pub fn progress(&self) -> String {
        let mut parts = Vec::new();
        match self.budgets.steps {
            Some(max) => parts.push(format!("step {}/{max}", self.step.max(1))),
            None => parts.push(format!("step {}", self.step.max(1))),
        }
        if self.output_tokens > 0 {
            parts.push(format!("{} tok", human_count(self.output_tokens)));
        }
        match self.budgets.time {
            Some(limit) => parts.push(format!(
                "{}/{}",
                human_duration(self.elapsed()),
                human_duration(limit)
            )),
            None => parts.push(human_duration(self.elapsed())),
        }
        parts.join(" · ")
    }

    /// The final report, as plain lines for the caller to draw in a card.
    /// Every line is a fact the loop reported, not something the model claimed.
    pub fn report(&self, reason: DoneReason) -> Vec<String> {
        let mut out = Vec::new();
        out.push(stop_line(reason, &self.budgets, self.step));
        out.push(String::new());

        let steps = match self.budgets.steps {
            Some(max) => format!("{}/{max}", self.step),
            None => self.step.to_string(),
        };
        out.push(format!(
            "Steps {steps} · {} generated tokens · {} elapsed",
            human_count(self.output_tokens),
            human_duration(self.elapsed())
        ));

        let touched = self.written.len() + self.edited.len();
        if touched == 0 {
            out.push("Files changed: none".to_string());
        } else {
            out.push(format!(
                "Files changed: {touched} ({} created, {} edited)",
                self.written.len(),
                self.edited.len()
            ));
            for f in self.written.iter().map(|f| format!("  + {f}")).chain(
                self.edited.iter().map(|f| format!("  ~ {f}")),
            ) {
                out.push(f);
            }
        }

        out.push(format!(
            "Shell commands: {} ok, {} failed",
            self.shell_ok,
            self.shell_failed.len()
        ));
        for f in &self.shell_failed {
            out.push(format!("  ! {f}"));
        }
        for f in &self.tool_failures {
            out.push(format!("  ! {f}"));
        }

        out.push(String::new());
        out.push(match reason {
            DoneReason::Completed => {
                "The model ended its turn on its own. Verify by hand: this report counts \
                 actions, it does not judge whether the goal was met."
                    .to_string()
            }
            DoneReason::Cancelled => {
                "You stopped the run; anything above already happened on disk.".to_string()
            }
            DoneReason::Failed => {
                "The backend failed mid-run; work listed above already happened on disk."
                    .to_string()
            }
            _ => "The goal was cut short by a budget — it is not finished. Raise the budget \
                  and re-run, or continue in the chat."
                .to_string(),
        });
        out
    }
}

fn stop_line(reason: DoneReason, budgets: &GoalBudgets, step: u32) -> String {
    match reason {
        DoneReason::Completed => "Stopped: model finished its turn".to_string(),
        DoneReason::StepLimit => format!(
            "Stopped: step budget reached ({} steps) — INCOMPLETE",
            budgets.steps.map_or(step.to_string(), |s| s.to_string())
        ),
        DoneReason::TokenBudget => format!(
            "Stopped: token budget reached ({}) — INCOMPLETE",
            budgets.output_tokens.map_or("budget".to_string(), human_count)
        ),
        DoneReason::TimeLimit => format!(
            "Stopped: time budget reached ({}) — INCOMPLETE",
            budgets.time.map_or("budget".to_string(), human_duration)
        ),
        DoneReason::Cancelled => "Stopped: interrupted by you — INCOMPLETE".to_string(),
        DoneReason::Failed => "Stopped: backend error — INCOMPLETE".to_string(),
    }
}

fn push_unique(list: &mut Vec<String>, value: String) {
    if !list.contains(&value) {
        list.push(value);
    }
}

fn first_line(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    crate::truncate_middle(line, 80)
}

/// The thing a call acted on: a path, a command, otherwise the tool's own name.
fn subject_of(name: &str, args_json: &str) -> String {
    let args = flashagent_llm::repair::effective_args(args_json, name);
    let pick = |key: &str| -> Option<String> {
        args.as_ref()?.get(key)?.as_str().map(|s| s.to_string())
    };
    let raw = match name {
        "run_shell" => pick("command").unwrap_or_else(|| "(poll/kill)".to_string()),
        _ => pick("path")
            .or_else(|| pick("pattern"))
            .or_else(|| pick("command"))
            .unwrap_or_default(),
    };
    if raw.is_empty() {
        return name.to_string();
    }
    crate::truncate_middle(&raw, 70)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev_started(id: &str, name: &str, args: &str) -> LoopEvent {
        LoopEvent::ToolStarted { id: id.into(), name: name.into(), args_json: args.into() }
    }
    fn ev_finished(id: &str, is_error: bool, result: &str) -> LoopEvent {
        LoopEvent::ToolFinished {
            id: id.into(),
            is_error,
            result_len: result.len(),
            result: Some(result.into()),
        }
    }

    #[test]
    fn defaults_apply_when_no_flags_are_given() {
        let (b, task) = parse_goal_command("fix the parser").unwrap();
        assert_eq!(b.steps, Some(DEFAULT_STEPS));
        assert_eq!(b.time, Some(DEFAULT_TIME));
        assert_eq!(b.output_tokens, None);
        assert_eq!(task, "fix the parser");
    }

    #[test]
    fn flags_are_parsed_in_both_spellings() {
        let (b, task) =
            parse_goal_command("--steps 40 --time=90s --tokens 200k rewrite the loop").unwrap();
        assert_eq!(b.steps, Some(40));
        assert_eq!(b.time, Some(Duration::from_secs(90)));
        assert_eq!(b.output_tokens, Some(200_000));
        assert_eq!(task, "rewrite the loop");
    }

    #[test]
    fn durations_accept_h_m_s_and_bare_minutes() {
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_duration("30m").unwrap(), Duration::from_secs(1800));
        assert_eq!(parse_duration("45min").unwrap(), Duration::from_secs(2700));
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("15").unwrap(), Duration::from_secs(900));
        assert!(parse_duration("soon").is_err());
        assert!(parse_duration("-5m").is_err());
        assert!(parse_duration("0").is_err());
    }

    #[test]
    fn a_flag_inside_the_task_is_task_text() {
        let (b, task) = parse_goal_command("make --tokens work in the cli").unwrap();
        assert_eq!(b.output_tokens, None, "a flag after the task must not be a budget");
        assert_eq!(task, "make --tokens work in the cli");
    }

    #[test]
    fn task_spacing_and_newlines_survive_flag_parsing() {
        let (_b, task) = parse_goal_command("--steps 3 fix   this:\n  line two").unwrap();
        assert_eq!(task, "fix   this:\n  line two");
    }

    #[test]
    fn bad_input_explains_itself_instead_of_running() {
        assert!(parse_goal_command("--steps").is_err());
        assert!(parse_goal_command("--steps 10").is_err(), "no task");
        assert!(parse_goal_command("--nope 1 do it").unwrap_err().contains("--nope"));
        assert!(parse_goal_command("--steps abc do it").unwrap_err().contains("number"));
        assert!(parse_goal_command("--tokens 0 do it").is_err());
    }

    #[test]
    fn ledger_records_only_what_the_loop_reported() {
        let mut l = GoalLedger::new("t".into(), GoalBudgets::default());
        l.on_event(&LoopEvent::StepStarted { step: 1, max_steps: Some(250) });
        l.on_event(&ev_started("1", "write_file", r#"{"path":"src/a.rs","content":"x"}"#));
        l.on_event(&ev_finished("1", false, "written"));
        l.on_event(&ev_started("2", "edit_file", r#"{"path":"src/b.rs"}"#));
        l.on_event(&ev_finished("2", false, "ok"));
        l.on_event(&ev_started("3", "run_shell", r#"{"command":"cargo test"}"#));
        l.on_event(&ev_finished("3", true, "exit code: 101\noutput:\nfailures"));
        l.on_event(&ev_started("4", "run_shell", r#"{"command":"cargo fmt"}"#));
        l.on_event(&ev_finished("4", false, "exit code: 0"));
        l.on_event(&LoopEvent::Usage(flashagent_llm::Usage {
            prompt: Some(9000),
            completion: Some(120),
            cached: None,
            mtp: None,
        }));

        let report = l.report(DoneReason::Completed).join("\n");
        assert!(report.contains("+ src/a.rs"), "{report}");
        assert!(report.contains("~ src/b.rs"), "{report}");
        assert!(report.contains("1 ok, 1 failed"), "{report}");
        assert!(report.contains("cargo test — exit code: 101"), "{report}");
        assert!(report.contains("120 generated tokens"), "{report}");
        assert_eq!(l.output_tokens(), 120, "prompt tokens must not be counted");
    }

    #[test]
    fn a_failed_write_is_not_reported_as_a_changed_file() {
        let mut l = GoalLedger::new("t".into(), GoalBudgets::default());
        l.on_event(&ev_started("1", "write_file", r#"{"path":"/etc/passwd"}"#));
        l.on_event(&ev_finished("1", true, "permission denied"));
        let report = l.report(DoneReason::Completed).join("\n");
        assert!(report.contains("Files changed: none"), "{report}");
        assert!(report.contains("write_file(/etc/passwd) — permission denied"), "{report}");
    }

    #[test]
    fn a_budget_stop_is_never_reported_as_finished() {
        let l = GoalLedger::new("t".into(), GoalBudgets::default());
        for reason in [DoneReason::StepLimit, DoneReason::TokenBudget, DoneReason::TimeLimit] {
            let report = l.report(reason).join("\n");
            assert!(report.contains("INCOMPLETE"), "{reason:?}: {report}");
            assert!(report.contains("not finished"), "{reason:?}: {report}");
        }
    }

    #[test]
    fn short_runs_do_not_report_themselves_as_instant() {
        assert_eq!(human_duration(Duration::from_millis(820)), "0.8s");
        assert_eq!(human_duration(Duration::from_secs(93)), "1m33s");
        assert_eq!(human_duration(Duration::from_secs(3600)), "1h0m");
    }

    #[test]
    fn progress_shows_the_step_against_its_budget() {
        let mut l = GoalLedger::new("t".into(), GoalBudgets::default());
        l.on_event(&LoopEvent::StepStarted { step: 12, max_steps: Some(250) });
        let p = l.progress();
        assert!(p.starts_with("step 12/250"), "{p}");
        assert!(p.contains("/1h0m"), "{p}");
    }

    #[test]
    fn wrapped_tool_arguments_still_name_the_file() {
        // Some models wrap arguments; the shared resolver must be used here too.
        let s = subject_of("write_file", r#"{"write_file":{"path":"src/x.rs"}}"#);
        assert_eq!(s, "src/x.rs");
    }
}
