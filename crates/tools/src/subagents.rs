//! Subagent tool factory for the built-in toolset.
//!
//! The parent agent loop is wrapped in [`PermissionedTools`]; subagents get the
//! same underlying `BuiltinTools` but a restricted tool subset and the same
//! permission state — so they inherit parent rights, never expand them
//! (PHILOSOPHY.md §6-7). The factory lives here because it needs the concrete
//! `BuiltinTools` to construct executors.

use std::sync::Arc;

use async_trait::async_trait;
use flashagent_core::{
    AgentRole, PermissionedTools, PermissionState, SubagentTool, SubagentToolFactory, ToolExec,
    ToolOutput,
};
use flashagent_llm::{ToolCall, ToolSpec};

use crate::BuiltinTools;

/// Combine several [`ToolExec`]s into one: the parent sees the built-in toolset
/// plus the `spawn_agent` subagent tool. `execute` dispatches by name to the
/// executor that owns it; specs are concatenated.
pub struct CompositeTools {
    executors: Vec<Arc<dyn ToolExec>>,
}

impl CompositeTools {
    /// Combine `executors`; order matters only for specs (first wins on a
    /// duplicate name).
    pub fn new(executors: Vec<Arc<dyn ToolExec>>) -> Self {
        Self { executors }
    }

    fn executor_for(&self, name: &str) -> Option<&Arc<dyn ToolExec>> {
        self.executors.iter().find(|e| e.specs().iter().any(|s| s.name == name))
    }
}

#[async_trait]
impl ToolExec for CompositeTools {
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        match self.executor_for(&call.name) {
            Some(e) => e.execute(call).await,
            None => ToolOutput {
                content: format!("unknown tool: {}", call.name),
                is_error: true,
                images: Vec::new(),
            },
        }
    }

    fn specs(&self) -> Vec<ToolSpec> {
        let mut specs = Vec::new();
        for e in &self.executors {
            specs.extend(e.specs());
        }
        specs
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Build a `CompositeTools` that wraps the built-in toolset and a `spawn_agent`
/// subagent tool sharing `llm` and `state`.
pub fn agent_tools(
    builtin: Arc<BuiltinTools>,
    llm: Arc<dyn flashagent_core::LlmSource>,
    state: Arc<PermissionState>,
) -> CompositeTools {
    let host = Arc::new(flashagent_core::SubagentHost::new(
        llm.clone(),
        Arc::new(BuiltinSubagentFactory::new(builtin.clone(), state.clone())),
    ));
    CompositeTools::new(vec![builtin, Arc::new(SubagentTool::new(host))])
}

/// Restrict a [`ToolExec`] to a subset of tool names. Unknown names are
/// filtered from specs and refused at execute (they never reach the inner
/// executor).
pub struct ToolSubset {
    inner: Arc<dyn ToolExec>,
    allowed: Vec<String>,
}

impl ToolSubset {
    /// Wrap `inner` so only `allowed` tools are visible and runnable.
    pub fn new(inner: Arc<dyn ToolExec>, allowed: &[String]) -> Self {
        Self { inner, allowed: allowed.to_vec() }
    }

    fn allows(&self, name: &str) -> bool {
        self.allowed.is_empty() || self.allowed.iter().any(|a| a == name)
    }
}

#[async_trait]
impl ToolExec for ToolSubset {
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        if !self.allows(&call.name) {
            return ToolOutput {
                content: format!("tool '{}' is not available to this subagent", call.name),
                is_error: true,
                images: Vec::new(),
            };
        }
        self.inner.execute(call).await
    }

    fn specs(&self) -> Vec<ToolSpec> {
        self.inner
            .specs()
            .into_iter()
            .filter(|s| self.allows(&s.name))
            .collect()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Factory that builds subagent executors over `BuiltinTools`, inheriting the
/// parent's permission state and restricted to the role's tool subset.
pub struct BuiltinSubagentFactory {
    tools: Arc<BuiltinTools>,
    state: Arc<PermissionState>,
}

impl BuiltinSubagentFactory {
    /// New factory over `tools`; children share `state` (the parent's rights).
    pub fn new(tools: Arc<BuiltinTools>, state: Arc<PermissionState>) -> Self {
        Self { tools, state }
    }
}

#[async_trait]
impl SubagentToolFactory for BuiltinSubagentFactory {
    fn build(&self, _role: &AgentRole, tools: &[String]) -> Arc<dyn ToolExec> {
        let subset = ToolSubset::new(self.tools.clone(), tools);
        // Wrap the restricted executor with the shared permission layer: the
        // child asks through the same gate, never expands rights.
        Arc::new(PermissionedTools::new(
            Arc::new(subset),
            Some(self.tools.clone()),
            self.state.clone(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flashagent_core::{ToolExec, ToolOutput};
    use flashagent_llm::{ToolCall, ToolSpec};

    // An inner executor recording every call it receives.
    struct Recorder;

    #[async_trait]
    impl ToolExec for Recorder {
        async fn execute(&self, call: &ToolCall) -> ToolOutput {
            ToolOutput { content: format!("ran {}", call.name), is_error: false, images: Vec::new() }
        }
        fn specs(&self) -> Vec<ToolSpec> {
            vec![
                ToolSpec { name: "read_file".into(), description: "r".into(), parameters_json: "{}".into() },
                ToolSpec { name: "write_file".into(), description: "w".into(), parameters_json: "{}".into() },
            ]
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[tokio::test]
    async fn subset_hides_and_blocks_disallowed_tools() {
        let inner: Arc<dyn ToolExec> = Arc::new(Recorder);
        let subset = ToolSubset::new(inner, &["read_file".to_string()]);

        // Only the allowed tool is advertised.
        let specs = subset.specs();
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["read_file"]);

        // Allowed tool runs.
        let out = subset
            .execute(&ToolCall { id: "t".into(), name: "read_file".into(), args_json: "{}".into() })
            .await;
        assert!(!out.is_error);
        assert_eq!(out.content, "ran read_file");

        // Disallowed tool is refused without reaching the inner executor.
        let out = subset
            .execute(&ToolCall { id: "t".into(), name: "write_file".into(), args_json: "{}".into() })
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("not available"));
    }

    #[tokio::test]
    async fn empty_subset_allows_everything() {
        let inner: Arc<dyn ToolExec> = Arc::new(Recorder);
        let subset = ToolSubset::new(inner, &[]);
        assert_eq!(subset.specs().len(), 2);
        let out = subset
            .execute(&ToolCall { id: "t".into(), name: "write_file".into(), args_json: "{}".into() })
            .await;
        assert!(!out.is_error);
    }
}
