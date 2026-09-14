//! Live plan tool (`update_plan`) for autonomous `/goal` runs.
//!
//! Lets the model keep a checklist of what it means to do and how far it has
//! gotten, shown next to the goal's own progress line. Only available during
//! `/goal`: an ordinary chat turn has no run long enough to need one, and the
//! model would otherwise spend tokens narrating a plan nobody asked for.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::Deserialize;

use crate::ToolError;

/// Where one step of the plan stands.
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

    /// Marker for a checklist line: `[ ]`, `[~]`, `[x]`.
    pub fn glyph(self) -> char {
        match self {
            Self::Pending => ' ',
            Self::InProgress => '~',
            Self::Completed => 'x',
        }
    }
}

/// One step of the plan.
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

/// The plan out of an `update_plan` call's raw arguments, read the same way
/// every other tool's arguments are (`effective_args`), so a model's own
/// naming for the field still works. Steps with no usable text are dropped.
///
/// `None` when the call named no `steps` field at all, or the arguments did
/// not even parse as an object; `Some(vec![])` when it named one and left it
/// empty (or every step in it was blank) — the caller treats those two
/// differently: one is a bad call, the other clears the plan.
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

/// Runs `update_plan`: only inside `/goal`, where a plan actually helps
/// something. Elsewhere it refuses itself, the same way `ask_user` refuses
/// itself the other way around inside `/goal`.
pub fn run_update_plan(is_goal_mode: &Arc<AtomicBool>, args_json: &str) -> Result<String, ToolError> {
    if !is_goal_mode.load(Ordering::Relaxed) {
        return Err(ToolError::Other("update_plan is only available during an autonomous /goal run".into()));
    }
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
    fn outside_goal_mode_the_tool_refuses_itself() {
        let flag = Arc::new(AtomicBool::new(false));
        let err = run_update_plan(&flag, r#"{"steps":[{"text":"a"}]}"#).unwrap_err();
        assert!(err.to_string().contains("only available during"), "{err}");
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
