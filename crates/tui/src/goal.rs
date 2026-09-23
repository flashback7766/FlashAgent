//! `/goal` limits and the end-of-run report. The ledger is built from
//! [`LoopEvent`]s, never from the model's account, so a run stopped by a
//! budget cannot read as finished.

use std::path::Path;
use std::time::{Duration, Instant};

use flashagent_core::{DoneReason, LoopEvent};

/// Completed steps between milestone commits.
pub const MILESTONE_COMMIT_INTERVAL: u32 = 10;

/// Each is off unless set in Settings → Goal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GoalBudgets {
    pub steps: Option<u32>,
    pub time: Option<Duration>,
    /// Completion tokens.
    pub output_tokens: Option<i64>,
}

impl GoalBudgets {
    /// Zero means no limit: nobody means a run that may take no steps.
    pub fn from_config(config: &flashagent_core::config::AppConfig) -> Self {
        Self {
            steps: config.goal_max_steps.filter(|n| *n > 0),
            time: config.goal_max_minutes.filter(|m| *m > 0).map(|m| Duration::from_secs(u64::from(m) * 60)),
            output_tokens: config.goal_max_output_tokens.filter(|t| *t > 0),
        }
    }

    /// Time and token budgets exist only for `/goal`.
    pub fn steps_only(steps: Option<u32>) -> Self {
        Self { steps, time: None, output_tokens: None }
    }

    pub fn is_unlimited(&self) -> bool {
        self.steps.is_none() && self.time.is_none() && self.output_tokens.is_none()
    }

    pub fn summary(&self) -> String {
        if self.is_unlimited() {
            return "no limits".to_string();
        }
        let mut parts = Vec::new();
        if let Some(n) = self.steps {
            parts.push(crate::plural(n as usize, "step", "steps"));
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

/// Spacing kept as typed. The old limit flags (`--steps 40`) now live in
/// Settings; a command written that way is told so.
pub fn parse_goal_task(rest: &str) -> Result<String, String> {
    let task = rest.trim();
    let first = task.split_whitespace().next().unwrap_or("");
    let flag = first.split('=').next().unwrap_or("");
    if matches!(flag, "--steps" | "--time" | "--tokens") {
        return Err(format!("{flag} is gone: /goal limits are set in Settings → Goal now. Usage: /goal <task>"));
    }
    if task.is_empty() {
        return Err("Nothing to do. Usage: /goal <task>".to_string());
    }
    Ok(task.to_string())
}

/// `93s` → `1m33s`. Under a minute keeps one decimal, so 800 ms is not "0s".
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

struct OpenToolCall {
    id: String,
    name: String,
    subject: String,
    files: Vec<String>,
    args_json: String,
}

/// What a `/goal` run actually did, accumulated from loop events.
pub struct GoalLedger {
    /// As the user phrased it.
    pub task: String,
    pub budgets: GoalBudgets,
    started: Instant,
    step: u32,
    output_tokens: i64,
    written: Vec<String>,
    edited: Vec<String>,
    shell_ok: usize,
    shell_failed: Vec<String>,
    tool_failures: Vec<String>,
    open_calls: Vec<OpenToolCall>,
    plan: Vec<flashagent_tools::plan_tool::PlanStep>,
}

impl GoalLedger {
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
            plan: Vec::new(),
        }
    }

    /// 0 before the first turn.
    pub fn step(&self) -> u32 {
        self.step
    }

    pub fn output_tokens(&self) -> i64 {
        self.output_tokens
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Deduplicated, written then edited, in first-touched order: what a milestone
    /// commit stages.
    pub fn changed_files(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        self.written.iter().chain(self.edited.iter()).filter(|f| seen.insert((*f).clone())).cloned().collect()
    }

    /// Empty until the model calls `update_plan`.
    pub fn plan(&self) -> &[flashagent_tools::plan_tool::PlanStep] {
        &self.plan
    }

    /// Returns `true` when the plan changed and needs a redraw.
    pub fn on_event(&mut self, ev: &LoopEvent) -> bool {
        match ev {
            LoopEvent::StepStarted { step, .. } => self.step = *step,
            LoopEvent::Usage(u) => self.output_tokens += u.completion.unwrap_or(0),
            LoopEvent::ToolStarted { id, name, args_json } => {
                let subject = subject_of(name, args_json);
                let files = files_of(name, args_json);
                self.open_calls.push(OpenToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    subject,
                    files,
                    args_json: args_json.clone(),
                });
            }
            LoopEvent::ToolFinished { id, is_error, result, .. } => {
                let Some(pos) = self.open_calls.iter().position(|call| call.id == *id) else {
                    return false;
                };
                let call = self.open_calls.remove(pos);
                match (call.name.as_str(), *is_error) {
                    ("write_file", false) => push_unique(&mut self.written, call.subject),
                    ("edit_file" | "patch_file", false) => {
                        // A batch edit changed every file it names.
                        let changed = if call.files.is_empty() {
                            vec![call.subject]
                        } else {
                            call.files
                        };
                        for file in changed {
                            push_unique(&mut self.edited, file);
                        }
                    }
                    ("run_shell", false) => self.shell_ok += 1,
                    ("run_shell", true) => {
                        let why = first_line(result.as_deref().unwrap_or(""));
                        self.shell_failed.push(format!("{} — {why}", call.subject));
                    }
                    ("update_plan", false) => {
                        if let Some(steps) = flashagent_tools::plan_tool::parse_plan(&call.args_json) {
                            self.plan = steps;
                            return true;
                        }
                    }
                    (_, true) => {
                        let why = first_line(result.as_deref().unwrap_or(""));
                        self.tool_failures.push(format!("{}({}) — {why}", call.name, call.subject));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        false
    }

    /// `step 12/250 · 4.2k tok · 3m05s/1h0m`
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

    /// Every line is a fact the loop reported, not a model claim.
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
            _ => "The goal was cut short by a limit — it is not finished. Raise the limit in \
                  Settings → Goal and re-run, or continue in the chat."
                .to_string(),
        });
        out
    }
}

/// Updated in place: `update_or_push_turn_system("plan:", ...)` finds the
/// previous one by this label.
pub fn format_plan(steps: &[flashagent_tools::plan_tool::PlanStep]) -> String {
    use flashagent_tools::plan_tool::PlanStatus;
    let done = steps.iter().filter(|s| s.status == PlanStatus::Completed).count();
    let mut out = format!("  \x1b[38;2;155;165;180mplan:\x1b[0m \x1b[38;2;160;155;145m{done}/{}\x1b[0m", steps.len());
    for step in steps {
        let color = match step.status {
            PlanStatus::Completed => "\x1b[38;2;145;205;140m",
            PlanStatus::InProgress => "\x1b[38;2;225;175;95m",
            PlanStatus::Pending => "\x1b[38;2;160;155;145m",
        };
        out.push_str(&format!("\n    {color}[{}]\x1b[0m {}", step.status.glyph(), step.text));
    }
    out
}

/// A git commit of what the goal changed so far, next to the `/rewind`
/// snapshots. Silent outside a repository or when there is nothing to commit
/// (the user may have committed mid-run).
pub fn commit_goal_milestone(ledger: &GoalLedger, cwd: &Path, completed_steps: u32) -> Option<String> {
    let files = ledger.changed_files();
    if files.is_empty() {
        return None;
    }
    let in_repo = std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .output()
        .is_ok_and(|o| o.status.success());
    if !in_repo {
        return None;
    }
    let mut add = std::process::Command::new("git");
    add.arg("add").arg("--").args(&files).current_dir(cwd);
    if !add.output().is_ok_and(|o| o.status.success()) {
        return None;
    }
    let subject = crate::truncate_middle(&ledger.task, 60);
    let message = format!("goal checkpoint: {subject} (step {completed_steps})");
    let commit = std::process::Command::new("git").args(["commit", "-m", &message]).current_dir(cwd).output().ok()?;
    if !commit.status.success() {
        return None;
    }
    Some(format!(
        "  \x1b[38;2;155;165;180mcheckpoint:\x1b[0m \x1b[38;2;225;230;240mcommitted {} after step {completed_steps}\x1b[0m",
        crate::plural(files.len(), "file", "files")
    ))
}

fn stop_line(reason: DoneReason, budgets: &GoalBudgets, step: u32) -> String {
    match reason {
        DoneReason::Completed => "Stopped: model finished its turn".to_string(),
        DoneReason::StepLimit => format!(
            "Stopped: step budget reached ({}) — INCOMPLETE",
            crate::plural(budgets.steps.unwrap_or(step) as usize, "step", "steps")
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

/// Its `path`, then each of its `files`.
fn files_of(name: &str, args_json: &str) -> Vec<String> {
    if !matches!(name, "edit_file" | "read_file") {
        return Vec::new();
    }
    let Some(args) = flashagent_llm::repair::effective_args(args_json, name) else {
        return Vec::new();
    };
    let single = args.get("path").and_then(|p| p.as_str());
    let batch = args.get("files").and_then(|f| f.as_array()).into_iter().flatten().filter_map(|f| f.get("path").and_then(|p| p.as_str()));
    single.into_iter().chain(batch).filter(|p| !p.is_empty()).map(str::to_string).collect()
}

/// What a call acted on: a path, a command, otherwise the tool's name.
fn subject_of(name: &str, args_json: &str) -> String {
    let args = flashagent_llm::repair::effective_args(args_json, name);
    let pick = |key: &str| -> Option<String> {
        args.as_ref()?.get(key)?.as_str().map(|s| s.to_string())
    };
    let raw = match name {
        "run_shell" => pick("command").unwrap_or_else(|| "(poll/kill)".to_string()),
        _ => pick("path")
            .or_else(|| {
                let files = files_of(name, args_json);
                (!files.is_empty()).then(|| files.join(", "))
            })
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
    fn a_goal_has_no_limits_unless_settings_give_it_some() {
        let b = GoalBudgets::from_config(&flashagent_core::config::AppConfig::default());
        assert!(b.is_unlimited(), "{b:?}");
        assert_eq!(b.summary(), "no limits");
    }

    #[test]
    fn limits_come_from_settings() {
        let cfg = flashagent_core::config::AppConfig {
            goal_max_steps: Some(40),
            goal_max_minutes: Some(30),
            goal_max_output_tokens: Some(200_000),
            ..Default::default()
        };
        let b = GoalBudgets::from_config(&cfg);
        assert_eq!(b.steps, Some(40));
        assert_eq!(b.time, Some(Duration::from_secs(1800)));
        assert_eq!(b.output_tokens, Some(200_000));
        assert_eq!(b.summary(), "40 steps · 30m00s max · 200.0k generated tokens");
        assert_eq!(GoalBudgets::steps_only(Some(1)).summary(), "1 step");
        assert!(stop_line(DoneReason::StepLimit, &GoalBudgets::steps_only(Some(1)), 1).contains("(1 step)"));
    }

    #[test]
    fn a_zero_limit_is_no_limit() {
        let cfg = flashagent_core::config::AppConfig {
            goal_max_steps: Some(0),
            goal_max_minutes: Some(0),
            goal_max_output_tokens: Some(0),
            ..Default::default()
        };
        assert!(GoalBudgets::from_config(&cfg).is_unlimited());
    }

    #[test]
    fn the_task_is_kept_as_typed() {
        assert_eq!(parse_goal_task("  fix   this:\n  line two  ").unwrap(), "fix   this:\n  line two");
        assert_eq!(
            parse_goal_task("make --tokens work in the cli").unwrap(),
            "make --tokens work in the cli",
            "a flag inside the task is task text"
        );
    }

    #[test]
    fn an_old_budget_flag_says_where_the_limits_went() {
        for old in ["--steps 40 fix it", "--time=30m fix it", "--tokens 200k fix it"] {
            let err = parse_goal_task(old).unwrap_err();
            assert!(err.contains("Settings"), "{old}: {err}");
        }
        assert!(parse_goal_task("   ").is_err(), "no task");
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
        let budgets = GoalBudgets { steps: Some(250), time: Some(Duration::from_secs(3600)), output_tokens: None };
        let mut l = GoalLedger::new("t".into(), budgets);
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

    #[test]
    fn a_batch_edit_reports_every_file_it_changed() {
        let mut l = GoalLedger::new("t".into(), GoalBudgets::default());
        l.on_event(&ev_started("1", "edit_file", r#"{"files":[{"path":"src/a.rs","edits":[]},{"filePath":"src/b.rs","edits":[]}]}"#));
        l.on_event(&ev_finished("1", false, "applied 2 edit(s) to 2 file(s)"));
        let report = l.report(DoneReason::Completed).join("\n");
        assert!(report.contains("~ src/a.rs") && report.contains("~ src/b.rs"), "{report}");
        assert!(report.contains("Files changed: 2"), "{report}");
    }

    #[test]
    fn changed_files_has_every_path_once() {
        let mut l = GoalLedger::new("t".into(), GoalBudgets::default());
        l.on_event(&ev_started("1", "write_file", r#"{"path":"a.txt"}"#));
        l.on_event(&ev_finished("1", false, "wrote a.txt"));
        l.on_event(&ev_started("2", "edit_file", r#"{"path":"b.txt","edits":[]}"#));
        l.on_event(&ev_finished("2", false, "applied 1 edit(s) to 1 file(s)"));
        l.on_event(&ev_started("3", "edit_file", r#"{"path":"a.txt","edits":[]}"#));
        l.on_event(&ev_finished("3", false, "applied 1 edit(s) to 1 file(s)"));
        let mut files = l.changed_files();
        files.sort();
        assert_eq!(files, vec!["a.txt".to_string(), "b.txt".to_string()]);
    }

    #[test]
    fn a_successful_update_plan_call_replaces_the_plan_and_reports_it_changed() {
        let mut l = GoalLedger::new("t".into(), GoalBudgets::default());
        assert!(l.plan().is_empty());

        let changed = l.on_event(&ev_started("1", "update_plan", r#"{"steps":[{"text":"read the notes"}]}"#));
        assert!(!changed, "nothing changes until the call finishes");
        let changed = l.on_event(&ev_finished("1", false, "Plan recorded: 1 step(s) (0 done, 0 in progress)"));
        assert!(changed);
        assert_eq!(l.plan(), &[flashagent_tools::plan_tool::PlanStep {
            text: "read the notes".into(),
            status: flashagent_tools::plan_tool::PlanStatus::Pending,
        }]);

        // A later call replaces the plan; it does not merge.
        l.on_event(&ev_started("2", "update_plan", r#"{"steps":[{"text":"a","status":"completed"},{"text":"b"}]}"#));
        let changed = l.on_event(&ev_finished("2", false, "Plan recorded: 2 step(s) (1 done, 0 in progress)"));
        assert!(changed);
        assert_eq!(l.plan().len(), 2);
    }

    #[test]
    fn a_failed_update_plan_call_does_not_touch_the_plan() {
        let mut l = GoalLedger::new("t".into(), GoalBudgets::default());
        l.on_event(&ev_started("1", "update_plan", r#"{"steps":[{"text":"a"}]}"#));
        l.on_event(&ev_finished("1", false, "ok"));
        assert_eq!(l.plan().len(), 1);

        l.on_event(&ev_started("2", "update_plan", r#"{}"#));
        let changed = l.on_event(&ev_finished("2", true, "error: update_plan needs `steps`"));
        assert!(!changed);
        assert_eq!(l.plan().len(), 1, "the earlier plan must survive a rejected call");
    }

    #[test]
    fn format_plan_marks_each_step_by_its_status() {
        use flashagent_tools::plan_tool::{PlanStatus, PlanStep};
        let steps = vec![
            PlanStep { text: "done step".into(), status: PlanStatus::Completed },
            PlanStep { text: "doing step".into(), status: PlanStatus::InProgress },
            PlanStep { text: "next step".into(), status: PlanStatus::Pending },
        ];
        let text = format_plan(&steps);
        let plain = strip_ansi_for_test(&text);
        assert!(plain.contains("plan: 1/3"), "{plain}");
        assert!(plain.contains("[x] done step"), "{plain}");
        assert!(plain.contains("[~] doing step"), "{plain}");
        assert!(plain.contains("[ ] next step"), "{plain}");
    }

    fn strip_ansi_for_test(s: &str) -> String {
        let mut out = String::new();
        let mut in_escape = false;
        for c in s.chars() {
            if in_escape {
                if c == 'm' {
                    in_escape = false;
                }
            } else if c == '\x1b' {
                in_escape = true;
            } else {
                out.push(c);
            }
        }
        out
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git").args(args).current_dir(dir).output().expect("run git");
        assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["config", "user.email", "test@example.com"]);
        git(dir.path(), &["config", "user.name", "Test"]);
        std::fs::write(dir.path().join("README.md"), "hi\n").unwrap();
        git(dir.path(), &["add", "README.md"]);
        git(dir.path(), &["commit", "-q", "-m", "initial"]);
        dir
    }

    #[test]
    fn a_milestone_commit_stages_and_commits_what_the_goal_touched() {
        let repo = init_repo();
        std::fs::write(repo.path().join("a.txt"), "one\n").unwrap();
        let mut l = GoalLedger::new("rewrite the parser".into(), GoalBudgets::default());
        l.on_event(&ev_started("1", "write_file", r#"{"path":"a.txt"}"#));
        l.on_event(&ev_finished("1", false, "wrote a.txt"));

        let note = commit_goal_milestone(&l, repo.path(), 10);
        assert!(note.is_some(), "should report a checkpoint");
        assert!(note.unwrap().contains("committed 1 file"));

        let log = std::process::Command::new("git").args(["log", "--oneline"]).current_dir(repo.path()).output().unwrap();
        let log = String::from_utf8_lossy(&log.stdout);
        assert!(log.contains("goal checkpoint: rewrite the parser (step 10)"), "{log}");
        assert_eq!(log.lines().count(), 2, "one checkpoint on top of the initial commit: {log}");
    }

    #[test]
    fn no_files_touched_means_no_commit_and_no_message() {
        let repo = init_repo();
        let l = GoalLedger::new("t".into(), GoalBudgets::default());
        assert_eq!(commit_goal_milestone(&l, repo.path(), 10), None);
        let log = std::process::Command::new("git").args(["log", "--oneline"]).current_dir(repo.path()).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&log.stdout).lines().count(), 1, "only the initial commit");
    }

    #[test]
    fn outside_a_git_repository_nothing_happens() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        let mut l = GoalLedger::new("t".into(), GoalBudgets::default());
        l.on_event(&ev_started("1", "write_file", r#"{"path":"a.txt"}"#));
        l.on_event(&ev_finished("1", false, "wrote a.txt"));
        assert_eq!(commit_goal_milestone(&l, dir.path(), 10), None);
    }

    #[test]
    fn a_file_already_committed_by_the_user_is_not_reported_as_an_error() {
        let repo = init_repo();
        std::fs::write(repo.path().join("a.txt"), "one\n").unwrap();
        let mut l = GoalLedger::new("t".into(), GoalBudgets::default());
        l.on_event(&ev_started("1", "write_file", r#"{"path":"a.txt"}"#));
        l.on_event(&ev_finished("1", false, "wrote a.txt"));
        // Already committed: the second attempt is silent.
        git(repo.path(), &["add", "a.txt"]);
        git(repo.path(), &["commit", "-q", "-m", "user beat the checkpoint to it"]);
        assert_eq!(commit_goal_milestone(&l, repo.path(), 10), None);
    }
}
