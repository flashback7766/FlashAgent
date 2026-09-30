//! The model's own checklist for a long task, shown as it works. Available in
//! ordinary chat as well as under `/goal`: the model decides whether a task
//! deserves one, and it only spends tokens on a plan when it is genuinely doing
//! several things in order.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

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

#[derive(Debug, Default, Deserialize)]
struct RawStep {
    #[serde(default, alias = "step", alias = "title", alias = "description")]
    text: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawPlanArgs {
    #[serde(default, alias = "plan", alias = "todos", alias = "items")]
    steps: Option<Vec<RawStep>>,
}

/// Read through `effective_args`; blank steps are dropped. `None` when there
/// is no `steps` field or the arguments do not parse (a bad call);
/// `Some(vec![])` clears the plan.
pub fn parse_plan(args_json: &str) -> Option<Vec<PlanStep>> {
    let value = flashagent_llm::effective_args(args_json, "update_plan")?;
    let raw: RawPlanArgs = serde_json::from_value(value).ok()?;
    let had_field = raw.steps.is_some();
    let steps: Vec<PlanStep> = raw
        .steps
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let text = s.text?;
            let text = text.trim();
            (!text.is_empty()).then(|| PlanStep { text: text.to_string(), status: PlanStatus::parse(s.status.as_deref()) })
        })
        .collect();
    had_field.then_some(steps)
}

/// Records a plan. Available in ordinary chat as well as under `/goal`: a long
/// task is a long task whether or not someone asked for autonomy, and refusing
/// the call outside `/goal` meant a model that had planned the work silently had
/// nowhere to show it.
pub fn run_update_plan(_is_goal_mode: &Arc<AtomicBool>, args_json: &str) -> Result<String, ToolError> {
    let steps = parse_plan(args_json)
        .ok_or_else(|| ToolError::Other("update_plan needs `steps`: a list of {text, status}".into()))?;
    if steps.is_empty() {
        return Ok("Plan cleared".to_string());
    }
    let done = steps.iter().filter(|s| s.status == PlanStatus::Completed).count();
    let doing = steps.iter().filter(|s| s.status == PlanStatus::InProgress).count();
    Ok(format!("Plan recorded: {} step(s) ({done} done, {doing} in progress)", steps.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_is_kept_outside_a_goal_run_too() {
        // It used to refuse itself here, so a model that had planned a long task
        // in ordinary chat had nowhere to put the plan.
        let flag = Arc::new(AtomicBool::new(false));
        let msg = run_update_plan(&flag, r#"{"steps":[{"text":"a"}]}"#).unwrap();
        assert!(msg.contains("Plan recorded"), "{msg}");
    }

    #[test]
    fn a_plan_reports_how_many_steps_are_done_and_in_progress() {
        let flag = Arc::new(AtomicBool::new(true));
        let msg = run_update_plan(
            &flag,
            r#"{"steps":[
                {"text":"read the failing test","status":"completed"},
                {"text":"find the bug","status":"in_progress"},
                {"text":"fix it and rerun"}
            ]}"#,
        )
        .unwrap();
        assert!(msg.contains("3 step(s)"), "{msg}");
        assert!(msg.contains("1 done"), "{msg}");
        assert!(msg.contains("1 in progress"), "{msg}");
    }

    #[test]
    fn blank_steps_are_dropped_not_kept_as_empty_lines() {
        let steps = parse_plan(r#"{"steps":[{"text":"  "},{"text":"real step"},{"status":"pending"}]}"#).unwrap();
        assert_eq!(steps, vec![PlanStep { text: "real step".into(), status: PlanStatus::Pending }]);
    }

    #[test]
    fn an_empty_plan_clears_rather_than_erroring() {
        let flag = Arc::new(AtomicBool::new(true));
        let msg = run_update_plan(&flag, r#"{"steps":[]}"#).unwrap();
        assert_eq!(msg, "Plan cleared");
    }

    #[test]
    fn a_call_with_no_steps_field_at_all_is_an_error() {
        let flag = Arc::new(AtomicBool::new(true));
        let err = run_update_plan(&flag, r#"{}"#).unwrap_err();
        assert!(err.to_string().contains("needs `steps`"), "{err}");
    }

    #[test]
    fn aliases_and_status_synonyms_are_understood() {
        let steps = parse_plan(r#"{"plan":[{"step":"do the thing","status":"done"},{"title":"next","status":"doing"}]}"#).unwrap();
        assert_eq!(steps[0], PlanStep { text: "do the thing".into(), status: PlanStatus::Completed });
        assert_eq!(steps[1], PlanStep { text: "next".into(), status: PlanStatus::InProgress });
    }

    #[test]
    fn wrapped_arguments_are_still_understood() {
        // Some models wrap arguments in the tool's own name.
        let steps = parse_plan(r#"{"update_plan":{"steps":[{"text":"a"}]}}"#).unwrap();
        assert_eq!(steps, vec![PlanStep { text: "a".into(), status: PlanStatus::Pending }]);
    }
}
