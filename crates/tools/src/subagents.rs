//! Subagent tool factory. Children get the same `BuiltinTools`, a restricted
//! tool subset and the parent's permission state, so they never gain rights.

use std::sync::Arc;

use async_trait::async_trait;
use flashagent_core::{
    AgentRole, PermissionedTools, PermissionState, SubagentToolFactory, ToolExec,
    ToolOutput,
};
use flashagent_llm::{ToolCall, ToolSpec};

use crate::BuiltinTools;

/// The built-in toolset plus `spawn_agent`. `execute` dispatches by name.
pub struct CompositeTools {
    executors: Vec<Arc<dyn ToolExec>>,
}

impl CompositeTools {
    /// Order matters only for specs: the first wins on a duplicate name.
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

/// The built-in toolset plus `spawn_agent`. `execute` dispatches by name.
///
/// `events` is where a running child's steps are sent for the app to show;
/// `finished` is where a child that has answered says so, so the answer can
/// reach the model as a notice instead of a tool result the parent waited for.
pub fn agent_tools(
    builtin: Arc<BuiltinTools>,
    llm: Arc<dyn flashagent_core::LlmSource>,
    state: Arc<PermissionState>,
    events: Option<tokio::sync::mpsc::UnboundedSender<flashagent_core::SubagentEvent>>,
) -> (
    CompositeTools,
    tokio::sync::mpsc::UnboundedReceiver<flashagent_core::SubagentOutbound>,
) {
    // The factory and the host must be given the same mailbox: a message the
    // parent writes and a message a child writes have to arrive in one place.
    let (outbound, done) = tokio::sync::mpsc::unbounded_channel();
    let mailbox = Arc::new(flashagent_core::Mailbox::new(outbound.clone()));
    let factory = Arc::new(BuiltinSubagentFactory::with_mailbox(builtin.clone(), state.clone(), mailbox.clone()));
    let host = flashagent_core::SubagentHost::with_shared(llm, factory, events, outbound);
    // The parent gets the two tools that only make sense at the top: recording
    // what it made of a report, and writing to whoever is running.
    let review = host.review_tool();
    let talk = Arc::new(flashagent_core::MessageTool::new(mailbox, "parent", "the agent you are talking to"));
    let spawn = Arc::new(flashagent_core::SubagentTool::new(Arc::new(host)));
    let executors: Vec<Arc<dyn ToolExec>> = vec![builtin, spawn, review, talk];
    (CompositeTools::new(executors), done)
}

/// Other names are hidden from specs and refused before reaching `inner`.
pub struct ToolSubset {
    inner: Arc<dyn ToolExec>,
    allowed: Vec<String>,
}

impl ToolSubset {
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

pub struct BuiltinSubagentFactory {
    tools: Arc<BuiltinTools>,
    state: Arc<PermissionState>,
    /// Shared with the host, so a message finds the child that was meant.
    mailbox: Option<Arc<flashagent_core::Mailbox>>,
}

impl BuiltinSubagentFactory {
    /// Children share `state`, the parent's rights.
    pub fn new(
        tools: Arc<BuiltinTools>,
        state: Arc<PermissionState>,
    ) -> Self {
        Self { tools, state, mailbox: None }
    }

    /// With the mailbox the children write through. The host builds one and
    /// hands it here, so a child and its parent share one.
    pub fn with_mailbox(
        tools: Arc<BuiltinTools>,
        state: Arc<PermissionState>,
        mailbox: Arc<flashagent_core::Mailbox>,
    ) -> Self {
        Self { tools, state, mailbox: Some(mailbox) }
    }
}

impl SubagentToolFactory for BuiltinSubagentFactory {
    fn build(&self, id: &str, role: &AgentRole, tools: &[String]) -> Arc<dyn ToolExec> {
        // An empty list means the parent's full toolset, so a coder gets
        // everything. A role that names its tools is fenced in: `ToolSubset`
        // hides them from the child's specs and refuses them if the child
        // calls one anyway, which is what makes "a researcher cannot write" a
        // boundary instead of a request in a prompt.
        let subset = ToolSubset::new(self.tools.clone(), tools);
        // A child can only be given the tool to write to the others when it was
        // built with their mailbox; without one it works alone, which is still
        // a valid way to run a task.
        let mut executors: Vec<Arc<dyn ToolExec>> = vec![Arc::new(subset)];
        if let Some(mailbox) = self.mailbox.clone() {
            executors.push(Arc::new(flashagent_core::MessageTool::new(mailbox, id, &role.name)));
        }
        // The child asks through the same gate, and may talk to anyone in the
        // task: a sibling that found something, or the parent.
        Arc::new(PermissionedTools::new(
            Arc::new(CompositeTools::new(executors)),
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

        let specs = subset.specs();
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["read_file"]);

        let out = subset
            .execute(&ToolCall { id: "t".into(), name: "read_file".into(), args_json: "{}".into() })
            .await;
        assert!(!out.is_error);
        assert_eq!(out.content, "ran read_file");

        // Refused without reaching the inner executor.
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

    #[tokio::test]
    async fn a_role_that_may_not_write_is_refused_even_when_it_tries() {
        // The guarantee a role name makes to the user: a researcher that
        // decides mid-run to write something is stopped by the subset, not by
        // its prompt. This is the check behind that promise.
        let inner: Arc<dyn ToolExec> = Arc::new(Recorder);
        let role = flashagent_core::subagents::SubagentHost::new(
            std::sync::Arc::new(Silent),
            std::sync::Arc::new(PassthroughFactory),
        )
        .role("researcher");
        let subset = ToolSubset::new(inner, &role.tools);

        assert!(subset.specs().iter().any(|s| s.name == "read_file"), "a researcher may read");
        assert!(!subset.specs().iter().any(|s| s.name == "write_file"), "and may not even see the tool");

        for forbidden in ["write_file", "edit_file", "run_shell", "patch_file"] {
            let out = subset
                .execute(&ToolCall { id: "t".into(), name: forbidden.into(), args_json: "{}".into() })
                .await;
            assert!(out.is_error, "{forbidden} must be refused to a researcher");
            assert!(out.content.contains("not available"), "{forbidden}: {}", out.content);
        }
    }

    struct Silent;

    #[async_trait]
    impl flashagent_core::LlmSource for Silent {
        async fn turn(
            &self,
            _messages: &[flashagent_llm::ChatMessage],
            _tools: &[ToolSpec],
        ) -> Result<
            futures::stream::BoxStream<'static, Result<flashagent_llm::LlmEvent, flashagent_llm::LlmError>>,
            flashagent_llm::LlmError,
        > {
            Ok(Box::pin(futures::stream::pending()))
        }
    }

    struct PassthroughFactory;

    impl flashagent_core::SubagentToolFactory for PassthroughFactory {
        fn build(&self, _id: &str, _role: &AgentRole, _tools: &[String]) -> Arc<dyn ToolExec> {
            Arc::new(Recorder)
        }
    }
}
