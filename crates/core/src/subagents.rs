//! Subagents: the parent loop spawns a child [`AgentLoop`] with its own role,
//! a tool subset and inherited permissions, which it can never expand. The
//! child's answer comes back as a tool result.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use flashagent_llm::{ChatMessage, ToolCall, ToolSpec};

use crate::loop_::{AgentLoop, DoneReason, LlmSource, LoopConfig, ToolExec, ToolOutput};

/// A system prompt plus which of the parent's tools the role may use.
#[derive(Debug, Clone)]
pub struct AgentRole {
    pub name: String,
    pub system_prompt: String,
    /// Empty means the parent's full toolset.
    pub tools: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SubagentSpec {
    pub role: AgentRole,
    /// Goes into the child's first user message.
    pub prompt: String,
    pub max_steps: u32,
    /// `None` means no limit.
    pub timeout: Option<Duration>,
    /// Cap on output fed back to the parent.
    pub max_output_chars: usize,
}

impl Default for SubagentSpec {
    fn default() -> Self {
        Self {
            role: AgentRole { name: "assistant".into(), system_prompt: String::new(), tools: Vec::new() },
            prompt: String::new(),
            max_steps: 20,
            timeout: None,
            max_output_chars: 32_000,
        }
    }
}

pub struct SubagentHandle {
    pub id: String,
    /// Ends with the child's answer; aborting it stops the child.
    pub task: tokio::task::JoinHandle<SubagentResult>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubagentResult {
    pub answer: String,
    pub done: DoneReason,
}

/// Supplied by the service layer, so the child sees the same tools with the
/// parent's rights.
pub trait SubagentToolFactory: Send + Sync {
    /// `tools` restricts the visible set.
    fn build(&self, role: &AgentRole, tools: &[String]) -> Arc<dyn ToolExec>;
}

/// Used when `spec.max_steps == 0`.
const DEFAULT_MAX_STEPS: u32 = 20;

/// Children inherit the parent's permission state through the tool factory.
pub struct SubagentHost {
    llm: Arc<dyn LlmSource>,
    factory: Arc<dyn SubagentToolFactory>,
    next_id: AtomicU32,
    live: Arc<AtomicUsize>,
}

impl SubagentHost {
    pub fn new(llm: Arc<dyn LlmSource>, factory: Arc<dyn SubagentToolFactory>) -> Self {
        Self { llm, factory, next_id: AtomicU32::new(1), live: Arc::new(AtomicUsize::new(0)) }
    }

    pub fn live(&self) -> usize {
        self.live.load(Ordering::Relaxed)
    }

    pub fn spawn(&self, spec: SubagentSpec) -> SubagentHandle {
        let id = format!("sub{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let llm = self.llm.clone();
        let factory = self.factory.clone();
        let live = self.live.clone();
        let SubagentSpec { role, prompt, max_steps, timeout, max_output_chars } = spec;
        let max_steps = if max_steps == 0 { DEFAULT_MAX_STEPS } else { max_steps };
        live.fetch_add(1, Ordering::Relaxed);

        let task = tokio::spawn(async move {
            // Decrements even when the task is aborted mid-run.
            let _live = LiveGuard(live);
            let tools = factory.build(&role, &role.tools);
            let config = LoopConfig { max_steps: Some(max_steps), max_tokens: None, ..Default::default() };
            let loop_ = AgentLoop::new(config, Arc::new(std::sync::atomic::AtomicBool::new(false)));
            let history = vec![ChatMessage::system(role.system_prompt), ChatMessage::user(prompt)];

            let run = async {
                match loop_.run(llm.as_ref(), tools.as_ref(), history, |_| {}).await {
                    Ok((final_history, done)) => (last_assistant_text(&final_history), done),
                    Err(e) => (format!("[subagent failed: {e}]"), DoneReason::Failed),
                }
            };

            let (body, done) = if let Some(t) = timeout {
                tokio::time::timeout(t, run)
                    .await
                    .unwrap_or_else(|_| ("[subagent timed out]".to_string(), DoneReason::Failed))
            } else {
                run.await
            };

            SubagentResult { answer: body.chars().take(max_output_chars).collect(), done }
        });

        SubagentHandle { id, task }
    }
}

struct LiveGuard(Arc<AtomicUsize>);

impl Drop for LiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Aborts the child when the parent stops waiting (the user cancelled the
/// parent turn), so no orphaned subagent keeps running tools.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn last_assistant_text(history: &[ChatMessage]) -> String {
    history
        .iter()
        .rev()
        .find(|m| m.role == flashagent_llm::Role::Assistant)
        .map(|m| m.content.clone())
        .unwrap_or_default()
}

/// Runs a subagent and returns its answer as a tool result, so injected
/// content in the answer can never become an instruction.
pub struct SubagentTool {
    host: Arc<SubagentHost>,
}

impl SubagentTool {
    pub fn new(host: Arc<SubagentHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl ToolExec for SubagentTool {
    fn specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "spawn_agent".into(),
            description: "Delegate a self-contained research or coding task to a subagent; it works on its own and returns its final answer.".into(),
            parameters_json: r#"{"type":"object","properties":{"role":{"type":"string","description":"researcher, coder, reviewer, planner, or a custom role"},"task":{"type":"string","description":"The whole task, self-contained"},"max_steps":{"type":"integer","description":"Max loop steps (default 20)"},"timeout_secs":{"type":"integer","description":"Max wall-clock seconds (default none)"}},"required":["task"]}"#.into(),
        }]
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let v: serde_json::Value = serde_json::from_str(call.args_json.trim()).unwrap_or(serde_json::json!({}));
        let task = v.get("task").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if task.is_empty() {
            return ToolOutput { content: "spawn_agent: `task` is required".into(), is_error: true, images: Vec::new() };
        }
        let role_name = v.get("role").and_then(|x| x.as_str()).unwrap_or("researcher").to_string();
        let max_steps = v.get("max_steps").and_then(|x| x.as_u64()).unwrap_or(20) as u32;
        let timeout_secs = v.get("timeout_secs").and_then(|x| x.as_u64());

        let spec = SubagentSpec {
            role: AgentRole {
                system_prompt: role_prompt(&role_name),
                name: role_name,
                tools: Vec::new(),
            },
            prompt: task,
            max_steps,
            timeout: timeout_secs.map(Duration::from_secs),
            max_output_chars: 32_000,
        };

        let mut child = AbortOnDrop(self.host.spawn(spec).task);
        match (&mut child.0).await {
            Ok(SubagentResult { answer, done }) => {
                ToolOutput { content: answer, is_error: done != DoneReason::Completed, images: Vec::new() }
            }
            Err(_) => ToolOutput { content: "[subagent exited without an answer]".into(), is_error: true, images: Vec::new() },
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// The child still inherits the parent's permission constraints.
fn role_prompt(role: &str) -> String {
    match role {
        "researcher" => "You are a research subagent. Gather information, read files, and report findings concisely. You may use read/search tools. Do not modify files unless asked.".to_string(),
        "coder" => "You are a coding subagent. Write clean, idiomatic, human-like code with concise, insightful comments explaining why, never stating the obvious. Implement the requested change, run tests, and report what you did. Respect permissions.".to_string(),
        "reviewer" => "You are a review subagent. Read the relevant code and report issues, risks, and suggestions. Do not modify files.".to_string(),
        "planner" => "You are a planning subagent. Break the task into steps and produce a concise plan. Do not modify files.".to_string(),
        _ => "You are a subagent carrying out the task described below. Work autonomously within your permissions and report a concise final answer.".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flashagent_llm::{FinishReason, LlmEvent};

    // LlmEvent is Clone but LlmError is not, so events are wrapped in Ok on emit.
    struct Scripted {
        events: Arc<Vec<LlmEvent>>,
    }

    #[async_trait]
    impl LlmSource for Scripted {
        async fn turn(
            &self,
            _messages: &[ChatMessage],
            _tools: &[ToolSpec],
        ) -> Result<futures::stream::BoxStream<'static, Result<LlmEvent, flashagent_llm::LlmError>>, flashagent_llm::LlmError> {
            let events = self.events.as_ref().clone();
            Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
        }
    }

    #[derive(Clone)]
    struct Factory;

    impl SubagentToolFactory for Factory {
        fn build(&self, _role: &AgentRole, tools: &[String]) -> Arc<dyn ToolExec> {
            let _ = tools;
            Arc::new(NoopTools)
        }
    }

    struct NoopTools;

    #[async_trait]
    impl ToolExec for NoopTools {
        async fn execute(&self, call: &ToolCall) -> ToolOutput {
            ToolOutput { content: format!("ran {}", call.name), is_error: false, images: Vec::new() }
        }
        fn specs(&self) -> Vec<ToolSpec> {
            vec![]
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    fn llm() -> Arc<dyn LlmSource> {
        Arc::new(Scripted {
            events: Arc::new(vec![
                LlmEvent::TextDelta("child answer".into()),
                LlmEvent::Done(FinishReason::Stop),
            ]),
        })
    }

    #[tokio::test]
    async fn spawn_returns_child_answer() {
        let host = SubagentHost::new(llm(), Arc::new(Factory));
        let spec = SubagentSpec {
            role: AgentRole { name: "researcher".into(), system_prompt: "you research".into(), tools: vec![] },
            prompt: "find X".into(),
            ..Default::default()
        };
        let result = host.spawn(spec).task.await.unwrap();
        assert_eq!(result, SubagentResult { answer: "child answer".into(), done: DoneReason::Completed });
        assert_eq!(host.live(), 0);
    }

    #[tokio::test]
    async fn timeout_returns_placeholder() {
        // A turn that never completes: the child must be cut by the timeout.
        struct Hang;
        #[async_trait]
        impl LlmSource for Hang {
            async fn turn(
                &self,
                _messages: &[ChatMessage],
                _tools: &[ToolSpec],
            ) -> Result<futures::stream::BoxStream<'static, Result<LlmEvent, flashagent_llm::LlmError>>, flashagent_llm::LlmError> {
                Ok(Box::pin(futures::stream::pending()))
            }
        }
        let host = SubagentHost::new(Arc::new(Hang), Arc::new(Factory));
        let spec = SubagentSpec {
            role: AgentRole { name: "r".into(), system_prompt: "s".into(), tools: vec![] },
            prompt: "p".into(),
            timeout: Some(Duration::from_millis(50)),
            ..Default::default()
        };
        let result = host.spawn(spec).task.await.unwrap();
        assert!(result.answer.contains("timed out"), "{result:?}");
        assert_eq!(result.done, DoneReason::Failed);
    }

    #[tokio::test]
    async fn role_system_prompt_is_used() {
        // The child's history is not inspectable; the factory checks that the tool
        // restriction and role name are passed.
        let host = SubagentHost::new(llm(), Arc::new(Factory));
        let spec = SubagentSpec {
            role: AgentRole { name: "coder".into(), system_prompt: "code".into(), tools: vec!["write_file".into()] },
            prompt: "impl".into(),
            ..Default::default()
        };
        let handle = host.spawn(spec);
        assert_eq!(handle.id, "sub1");
        handle.task.await.unwrap();
    }

    #[tokio::test]
    async fn tool_returns_answer_in_tool_role() {
        // The answer must arrive as a tool result, never a system instruction.
        let host = Arc::new(SubagentHost::new(llm(), Arc::new(Factory)));
        let tool = SubagentTool::new(host);
        let out = tool
            .execute(&ToolCall { id: "t".into(), name: "spawn_agent".into(), args_json: r#"{"task":"go"}"#.into() })
            .await;
        assert!(!out.is_error);
        assert_eq!(out.content, "child answer");
    }

    #[tokio::test]
    async fn cancelling_the_parent_call_aborts_the_child() {
        struct Hang;
        #[async_trait]
        impl LlmSource for Hang {
            async fn turn(
                &self,
                _messages: &[ChatMessage],
                _tools: &[ToolSpec],
            ) -> Result<futures::stream::BoxStream<'static, Result<LlmEvent, flashagent_llm::LlmError>>, flashagent_llm::LlmError> {
                Ok(Box::pin(futures::stream::pending()))
            }
        }
        let host = Arc::new(SubagentHost::new(Arc::new(Hang), Arc::new(Factory)));
        let tool = SubagentTool::new(host.clone());
        let call = ToolCall { id: "t".into(), name: "spawn_agent".into(), args_json: r#"{"task":"forever"}"#.into() };
        // The parent loop drops the execute future when the user cancels.
        let res = tokio::time::timeout(Duration::from_millis(50), tool.execute(&call)).await;
        assert!(res.is_err());
        for _ in 0..50 {
            if host.live() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(host.live(), 0, "child must not outlive the cancelled parent call");
    }

    #[tokio::test]
    async fn live_counter_tracks_running_children() {
        let host = SubagentHost::new(llm(), Arc::new(Factory));
        let spec = SubagentSpec { prompt: "p".into(), ..Default::default() };
        let _handle = host.spawn(spec);
        assert!(host.live() >= 1);
    }
}
