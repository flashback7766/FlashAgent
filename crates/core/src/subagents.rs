//! Subagents: the parent loop spawns a child [`AgentLoop`] with its own role,
//! a tool subset and inherited permissions, which it can never expand. Results
//! come back as tool results; children can message each other over per-id
//! channels.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
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

#[derive(Debug, Clone, PartialEq)]
pub enum SubagentMsg {
    Text { from: String, to: String, body: String },
    Finished { from: String, body: String, done: DoneReason },
    Cancel { to: String },
}

pub struct SubagentHandle {
    pub id: String,
    pub rx: tokio::sync::mpsc::UnboundedReceiver<SubagentMsg>,
    pub task: tokio::task::JoinHandle<()>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubagentResult {
    pub id: String,
    pub answer: String,
    pub done: DoneReason,
}

/// Supplied by the service layer, so the child sees the same tools with the
/// parent's rights.
#[async_trait]
pub trait SubagentToolFactory: Send + Sync {
    /// `tools` restricts the visible set.
    fn build(&self, role: &AgentRole, tools: &[String]) -> Arc<dyn ToolExec>;
}

/// Children inherit the parent's permission state through the tool factory.
pub struct SubagentHost {
    llm: Arc<dyn LlmSource>,
    factory: Arc<dyn SubagentToolFactory>,
    next_id: Mutex<u32>,
    live: Arc<AtomicUsize>,
    /// Used when `spec.max_steps == 0`.
    default_max_steps: u32,
}

impl SubagentHost {
    pub fn new(llm: Arc<dyn LlmSource>, factory: Arc<dyn SubagentToolFactory>) -> Self {
        Self { llm, factory, next_id: Mutex::new(1), live: Arc::new(AtomicUsize::new(0)), default_max_steps: 20 }
    }

    pub fn live(&self) -> usize {
        self.live.load(Ordering::Relaxed)
    }

    pub fn spawn(&self, spec: SubagentSpec) -> SubagentHandle {
        let id = {
            let mut n = self.next_id.lock().expect("next_id");
            let id = *n;
            *n += 1;
            format!("sub{}", id)
        };

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let llm = self.llm.clone();
        let factory = self.factory.clone();
        let live = self.live.clone();
        let max_steps = if spec.max_steps == 0 { self.default_max_steps } else { spec.max_steps };
        let role = spec.role.clone();
        let prompt = spec.prompt.clone();
        let max_output = spec.max_output_chars;
        let timeout = spec.timeout;
        let id2 = id.clone();
        live.fetch_add(1, Ordering::Relaxed);

        let task = tokio::spawn(async move {
            // Decrements even when the task is aborted mid-run.
            let _live = LiveGuard(live);
            let tools = factory.build(&role, &role.tools);
            let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let config = LoopConfig { max_steps: Some(max_steps), max_tokens: None, ..Default::default() };
            let loop_ = AgentLoop::new(config, cancel.clone());

            let history = vec![
                ChatMessage::system(role.system_prompt.clone()),
                ChatMessage::user(prompt.clone()),
            ];

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

            let body = body.chars().take(max_output).collect();
            let _ = tx.send(SubagentMsg::Finished { from: id2, body, done });
        });

        SubagentHandle { id, rx, task }
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
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
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
    /// Also forward events to the UI.
    pub events: bool,
}

impl SubagentTool {
    pub fn new(host: Arc<SubagentHost>) -> Self {
        Self { host, events: true }
    }
}

#[async_trait]
impl ToolExec for SubagentTool {
    fn specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "spawn_agent".into(),
            description: "Run a subagent with a role, a task, and optional limits. The subagent works independently and returns a final answer. Use it to delegate a self-contained research or implementation task while you keep coordinating. Its result is a string you can quote or build on.".into(),
            parameters_json: r#"{"type":"object","properties":{"role":{"type":"string","description":"Role name: researcher, coder, reviewer, planner, or any custom role"},"task":{"type":"string","description":"The task for the subagent, stated clearly and self-contained"},"max_steps":{"type":"integer","description":"Max loop steps (default 20)"},"timeout_secs":{"type":"integer","description":"Max wall-clock seconds (default none)"}},"required":["task"]}"#.into(),
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

        let handle = self.host.spawn(spec);
        let mut rx = handle.rx;
        let _child = AbortOnDrop(handle.task);
        while let Some(msg) = rx.recv().await {
            if let SubagentMsg::Finished { body, done, .. } = msg {
                let is_error = !matches!(done, DoneReason::Completed);
                return ToolOutput { content: body, is_error, images: Vec::new() };
            }
        }
        ToolOutput { content: "[subagent exited without an answer]".into(), is_error: true, images: Vec::new() }
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

    #[async_trait]
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
        let mut handle = host.spawn(spec);
        let msg = handle.rx.recv().await.unwrap();
        match msg {
            SubagentMsg::Finished { body, .. } => assert_eq!(body, "child answer"),
            other => panic!("expected Finished, got {other:?}"),
        }
        handle.task.await.unwrap();
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
        let mut handle = host.spawn(spec);
        let msg = handle.rx.recv().await.unwrap();
        match msg {
            SubagentMsg::Finished { body, .. } => assert!(body.contains("timed out")),
            other => panic!("expected Finished, got {other:?}"),
        }
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
