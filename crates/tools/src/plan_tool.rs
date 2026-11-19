//! The model's own checklist for a long task, shown as it works. Available in
//! ordinary chat as well as under `/goal`: the model decides whether a task
//! deserves one, and it only spends tokens on a plan when it is genuinely doing
//! several things in order.
//!
//! The tool is stateless on purpose: the plan the screen shows is kept by the
//! UI, which applies every call to its own copy. `create` and `change` send
//! the whole list; `done` and `in_progress` name steps and move only their
//! status, so finishing a step costs one short call instead of a full rewrite.

use serde::Deserialize;

use crate::ToolError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
}

impl PlanStatus {
    fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::to_lowercase).as_deref() {
            Some("in_progress" | "in-progress" | "doing" | "current" | "active") => Self::InProgress,
            Some("completed" | "complete" | "done" | "finished") => Self::Completed,
            _ => Self::Pending,
        }
    }

    pub fn glyph(self) -> char {
        match self {
            // From the set the Windows-console font test checks, so a plan
            // renders on every terminal the app claims to support.
            Self::Pending => '·',
            Self::InProgress => '▸',
            Self::Completed => '√',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanStep {
    pub text: String,
    pub status: PlanStatus,
}

/// What a `done`/`in_progress` call names: a step by its place in the list or
/// by its exact text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepRef {
    Index(usize),
    Text(String),
}

/// One `plan` call, read through `effective_args`. A wrapped call under the
/// old name still opens, so a prompt that taught `update_plan` keeps working
/// through the rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanAction {
    /// The whole plan, in order; statuses default to pending.
    Create(Vec<PlanStep>),
    /// The whole list again, with texts, order or statuses edited.
    Change(Vec<PlanStep>),
    /// Move the named steps' status; texts stay as they are.
    Mark { status: PlanStatus, refs: Vec<StepRef> },
    Clear,
}

#[derive(Debug, Default, Deserialize)]
struct RawStep {
    #[serde(default, alias = "step", alias = "title", alias = "description")]
    text: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawPlanArgs {
    #[serde(default, alias = "action")]
    action: Option<String>,
    #[serde(default, alias = "plan", alias = "todos", alias = "items")]
    items: Option<Vec<RawStep>>,
    #[serde(default, alias = "step", alias = "item", alias = "targets", alias = "ids")]
    steps: Option<Vec<serde_json::Value>>,
}

/// One step from its raw fields: blanks are dropped, not kept as empty lines.
fn step_from(text: &Option<String>, status: &Option<String>) -> Option<PlanStep> {
    let text = text.as_deref()?.trim();
    (!text.is_empty()).then(|| PlanStep { text: text.to_string(), status: PlanStatus::parse(status.as_deref()) })
}

/// One step's reference, read out of `done`/`in_progress` arguments: a number
/// is a 0-based index, a string is the step's exact text.
fn step_ref(value: &serde_json::Value) -> Option<StepRef> {
    if let Some(n) = value.as_u64() {
        return Some(StepRef::Index(n as usize));
    }
    let text = value.as_str()?.trim().to_string();
    (!text.is_empty()).then_some(StepRef::Text(text))
}

/// Read the call. `None` when the arguments do not parse (a bad call).
pub fn parse_plan_args(args_json: &str) -> Option<PlanAction> {
    // The first read takes the call as the new tool shaped it; when that says
    // nothing — a wrapper under the old name reads as one unknown field — the
    // same arguments are read once more as the old tool sent them.
    fn read(value: serde_json::Value) -> Option<PlanAction> {
        let raw: RawPlanArgs = match serde_json::from_value(value) {
            Ok(raw) => raw,
            Err(_) => return None,
        };
        let action = raw.action.as_deref().map(str::to_lowercase);
        let items = || {
            raw.items
                .iter()
                .flatten()
                .filter_map(|s| step_from(&s.text, &s.status))
                .collect::<Vec<_>>()
        };
        match action.as_deref() {
            Some("create" | "new" | "start") => Some(PlanAction::Create(items())),
            Some("change" | "update" | "edit" | "replace") => Some(PlanAction::Change(items())),
            Some("done" | "complete") => {
                let refs = raw.steps.as_deref()?.iter().filter_map(step_ref).collect();
                Some(PlanAction::Mark { status: PlanStatus::Completed, refs })
            }
            Some("in_progress" | "in-progress" | "start step" | "begin") => {
                let refs = raw.steps.as_deref()?.iter().filter_map(step_ref).collect();
                Some(PlanAction::Mark { status: PlanStatus::InProgress, refs })
            }
            Some("clear" | "reset") => Some(PlanAction::Clear),
            // No action named: the old full-list shape, where the presence of a
            // list is the whole message — under `items`, or under `steps` as the
            // old name sent it. An empty list clears.
            None if raw.items.is_some() || raw.steps.is_some() => {
                let mut steps = items();
                steps.extend(raw.steps.iter().flatten().filter(|v| v.is_object()).filter_map(|v| {
                    let s: RawStep = serde_json::from_value(v.clone()).ok()?;
                    step_from(&s.text, &s.status)
                }));
                Some(if steps.is_empty() { PlanAction::Clear } else { PlanAction::Change(steps) })
            }
            _ => None,
        }
    }
    read(flashagent_llm::effective_args(args_json, "plan")?)
        .or_else(|| read(flashagent_llm::effective_args(args_json, "update_plan")?))
}

/// Applies a `done`/`in_progress` call to the plan the UI keeps: a step
/// matches its 0-based index or its exact text. Returns how many steps moved.
pub fn apply_mark(steps: &mut [PlanStep], status: PlanStatus, refs: &[StepRef]) -> usize {
    let mut moved = 0;
    for r in refs {
        let hit = match r {
            StepRef::Index(i) => steps.get_mut(*i),
            StepRef::Text(text) => steps.iter_mut().find(|s| s.text == *text),
        };
        if let Some(step) = hit {
            step.status = status;
            moved += 1;
        }
    }
    moved
}

/// Applies a whole call to the plan the UI keeps, and says whether anything
/// changed. `create` and `change` replace the list; a mark moves only the
/// steps it named, so a text that matched nothing leaves the plan alone.
pub fn apply_plan_action(plan: &mut Vec<PlanStep>, action: PlanAction) -> bool {
    match action {
        PlanAction::Create(steps) | PlanAction::Change(steps) => {
            let changed = *plan != steps;
            *plan = steps;
            changed
        }
        PlanAction::Mark { status, refs } => apply_mark(plan, status, &refs) > 0,
        PlanAction::Clear => {
            let changed = !plan.is_empty();
            plan.clear();
            changed
        }
    }
}

/// The short line the transcript keeps while the plan box shows on screen:
/// one row, whatever the plan holds.
pub fn format_plan_summary(steps: &[PlanStep]) -> String {
    if steps.is_empty() {
        return "Plan cleared".to_string();
    }
    let done = steps.iter().filter(|s| s.status == PlanStatus::Completed).count();
    let doing = steps.iter().filter(|s| s.status == PlanStatus::InProgress).count();
    // The lowercase label is the transcript's own convention: the block is
    // found and updated in place by the prefix it starts with.
    format!("plan: {} step(s) · {} done · {} in progress", steps.len(), done, doing)
}

/// Runs a call. The plan itself is kept by the UI; this answers with what the
/// call did, in words the model can check against what it sent.
pub fn run_plan(args_json: &str) -> Result<String, ToolError> {
    let Some(action) = parse_plan_args(args_json) else {
        return Err(ToolError::Other(
            "plan needs `action` (create, change, done, in_progress or clear); create and change take `items`, done and in_progress take `steps`".into(),
        ));
    };
    match action {
        PlanAction::Clear => Ok("Plan cleared".to_string()),
        PlanAction::Create(steps) if steps.is_empty() => {
            Err(ToolError::Other("plan create needs `items`: a list of {text, status}".into()))
        }
        PlanAction::Create(steps) => Ok(format!("Plan created: {} step(s)", steps.len())),
        PlanAction::Change(steps) => {
            let done = steps.iter().filter(|s| s.status == PlanStatus::Completed).count();
            let doing = steps.iter().filter(|s| s.status == PlanStatus::InProgress).count();
            Ok(format!("Plan changed: {} step(s) ({done} done, {doing} in progress)", steps.len()))
        }
        PlanAction::Mark { status, refs } => {
            let what = match status {
                PlanStatus::Completed => "done",
                PlanStatus::InProgress => "in progress",
                PlanStatus::Pending => "pending",
            };
            Ok(format!("Marked {} step(s) {}", refs.len(), what))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_change_take_the_whole_list() {
        let a = parse_plan_args(r#"{"action":"create","items":[{"text":"read the notes"},{"text":"fix it","status":"done"}]}"#).unwrap();
        assert_eq!(
            a,
            PlanAction::Create(vec![
                PlanStep { text: "read the notes".into(), status: PlanStatus::Pending },
                PlanStep { text: "fix it".into(), status: PlanStatus::Completed },
            ])
        );
        let b = parse_plan_args(r#"{"action":"change","items":[{"text":"only this now"}]}"#).unwrap();
        assert_eq!(b, PlanAction::Change(vec![PlanStep { text: "only this now".into(), status: PlanStatus::Pending }]));
    }

    #[test]
    fn a_mark_names_steps_by_index_or_text() {
        let a = parse_plan_args(r#"{"action":"done","steps":[0,"fix it"]}"#).unwrap();
        assert_eq!(
            a,
            PlanAction::Mark { status: PlanStatus::Completed, refs: vec![StepRef::Index(0), StepRef::Text("fix it".into())] }
        );
        let b = parse_plan_args(r#"{"action":"in_progress","steps":["find the bug"]}"#).unwrap();
        assert_eq!(
            b,
            PlanAction::Mark { status: PlanStatus::InProgress, refs: vec![StepRef::Text("find the bug".into())] }
        );
    }

    #[test]
    fn clear_stands_alone_and_the_old_name_still_opens() {
        assert_eq!(parse_plan_args(r#"{"action":"clear"}"#), Some(PlanAction::Clear));
        // A prompt from before the rename wraps the call under the old name.
        let old = parse_plan_args(r#"{"update_plan":{"steps":[{"text":"a"}]}}"#).unwrap();
        assert_eq!(old, PlanAction::Change(vec![PlanStep { text: "a".into(), status: PlanStatus::Pending }]));
        // The old shape with no steps at all cleared the plan, and still does.
        assert_eq!(parse_plan_args(r#"{"update_plan":{"steps":[]}}"#), Some(PlanAction::Clear));
    }

    #[test]
    fn a_call_with_no_action_and_no_items_is_a_bad_call() {
        assert_eq!(parse_plan_args(r#"{}"#), None);
        assert_eq!(parse_plan_args(r#"{"action":"done"}"#), None, "a mark without steps names nothing");
    }

    #[test]
    fn apply_mark_moves_only_the_steps_that_matched() {
        let mut steps = vec![
            PlanStep { text: "read".into(), status: PlanStatus::Pending },
            PlanStep { text: "fix".into(), status: PlanStatus::Pending },
        ];
        let moved = apply_mark(&mut steps, PlanStatus::Completed, &[StepRef::Index(0), StepRef::Text("gone".into())]);
        assert_eq!(moved, 1, "the text that matched nothing is not a step");
        assert_eq!(steps[0].status, PlanStatus::Completed);
        assert_eq!(steps[1].status, PlanStatus::Pending);
    }

    #[test]
    fn the_answers_say_what_happened() {
        assert_eq!(run_plan(r#"{"action":"create","items":[{"text":"a"},{"text":"b"}]}"#).unwrap(), "Plan created: 2 step(s)");
        assert!(run_plan(r#"{"action":"change","items":[{"text":"a","status":"done"},{"text":"b"}]}"#)
            .unwrap()
            .contains("1 done"));
        assert_eq!(run_plan(r#"{"action":"done","steps":["a"]}"#).unwrap(), "Marked 1 step(s) done");
        assert_eq!(run_plan(r#"{"action":"clear"}"#).unwrap(), "Plan cleared");
        assert!(run_plan(r#"{}"#).is_err());
        assert!(run_plan(r#"{"action":"create"}"#).is_err(), "a create without items makes no plan");
    }

    #[test]
    fn the_summary_is_one_line_whatever_the_plan_holds() {
        let steps = vec![
            PlanStep { text: "a".into(), status: PlanStatus::Completed },
            PlanStep { text: "b".into(), status: PlanStatus::InProgress },
            PlanStep { text: "c".into(), status: PlanStatus::Pending },
        ];
        assert_eq!(format_plan_summary(&steps), "plan: 3 step(s) · 1 done · 1 in progress");
        assert_eq!(format_plan_summary(&[]), "Plan cleared");
    }

    #[test]
    fn aliases_and_status_synonyms_are_understood() {
        let a = parse_plan_args(r#"{"action":"update","todos":[{"step":"do the thing","status":"done"},{"title":"next","status":"doing"}]}"#).unwrap();
        assert_eq!(
            a,
            PlanAction::Change(vec![
                PlanStep { text: "do the thing".into(), status: PlanStatus::Completed },
                PlanStep { text: "next".into(), status: PlanStatus::InProgress },
            ])
        );
    }
}
