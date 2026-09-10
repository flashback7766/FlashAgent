//! Permission layer: four modes (Planning / Manual / AcceptEdits / Bypass),
//! action categories, narrow shell allow-rules with chain parsing, session
//! rules and an approval-gate trait the UI implements.
//!
//! The layer wraps any [`ToolExec`] ([`PermissionedTools`]); the loop itself
//! stays permission-agnostic. Canon: PHILOSOPHY.md §6.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use flashagent_llm::{ToolCall, ToolSpec};

use crate::loop_::{ToolExec, ToolOutput};

/// Permission mode. Canon order:
/// 1. Planning — read-only research, denies writes and shell execution.
/// 2. Manual — confirms every non-read action (both writes and commands).
/// 3. AcceptEdits (default out-of-box) — auto-approves file writes and edits; terminal commands still require confirmation.
/// 4. Bypass (Accept All) — all actions allowed without confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PermissionMode {
    /// Read-only research: writes and shell execution are denied.
    Planning,
    /// Ask before every non-read action.
    Manual,
    /// Auto-approve file writes and edits; terminal commands still require confirmation.
    AcceptEdits,
    /// Everything allowed, nothing asked. User's explicit choice (Accept All).
    Bypass,
}

impl PermissionMode {
    /// Next mode in cycle: 1. Planning -> 2. Manual -> 3. AcceptEdits -> 4. Bypass -> 1. Planning.
    pub fn next(self) -> Self {
        match self {
            Self::Planning => Self::Manual,
            Self::Manual => Self::AcceptEdits,
            Self::AcceptEdits => Self::Bypass,
            Self::Bypass => Self::Planning,
        }
    }

    /// Previous mode in cycle: 1. Planning -> 4. Bypass -> 3. AcceptEdits -> 2. Manual -> 1. Planning.
    pub fn prev(self) -> Self {
        match self {
            Self::Planning => Self::Bypass,
            Self::Manual => Self::Planning,
            Self::AcceptEdits => Self::Manual,
            Self::Bypass => Self::AcceptEdits,
        }
    }

    /// Human-friendly display label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Planning => "Planning",
            Self::Manual => "Manual",
            Self::AcceptEdits => "Accept Edits",
            Self::Bypass => "Accept All",
        }
    }
}

/// What kind of action a tool call is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    /// read_file, list_dir, glob, grep — always allowed.
    Read,
    /// write_file, edit_file — diff preview required.
    Write,
    /// run_shell — narrow prefix rules.
    Shell,
    /// web_fetch, web_search — read-only network.
    Net,
    /// Unknown/external (MCP and friends) — preview + approval in Manual.
    Mcp,
}

impl Category {
    /// Classify by tool name; unknown names are treated as MCP (external).
    pub fn from_tool(tool: &str) -> Self {
        match tool {
            "read_file" | "list_dir" | "glob" | "grep" | "outline_file" | "git_status" | "git_diff"
            | "env_info" | "memory_read" | "ask_user" => Category::Read,
            "write_file" | "edit_file" | "patch_file" | "memory_create" | "memory_update"
            | "memory_remove" => Category::Write,
            "run_shell" => Category::Shell,
            "web_fetch" | "web_search" => Category::Net,
            _ => Category::Mcp,
        }
    }
}

/// What the permission layer decided for one call.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Run it.
    Allow,
    /// Ask the user; carries the write diff preview when known.
    NeedApproval { diff: Option<String> },
    /// Refuse without asking.
    Deny(String),
}

/// Everything the UI needs to render an approval card.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    /// Tool name.
    pub tool: String,
    /// Raw JSON arguments.
    pub args_json: String,
    /// Action category.
    pub category: Category,
    /// Unified diff for write/edit calls, if computable.
    pub diff: Option<String>,
}

/// User's answer to an approval card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Run this once.
    Allow,
    /// Refuse this call.
    Deny,
}

/// Async gate implemented by the UI/service layer: shows the card, waits.
#[async_trait]
pub trait ApprovalGate: Send + Sync {
    /// Ask the user (or policy) about `req`.
    async fn approve(&self, req: &ApprovalRequest) -> Decision;
}

/// Gate that denies everything needing approval — for tests and headless runs.
#[derive(Default)]
pub struct DenyAllGate;

#[async_trait]
impl ApprovalGate for DenyAllGate {
    async fn approve(&self, _req: &ApprovalRequest) -> Decision {
        Decision::Deny
    }
}

/// Gate that approves everything needing approval — for tests.
#[derive(Default)]
pub struct AllowAllGate;

#[async_trait]
impl ApprovalGate for AllowAllGate {
    async fn approve(&self, _req: &ApprovalRequest) -> Decision {
        Decision::Allow
    }
}

/// Session-scoped rules: the "Always" button and narrow shell prefixes.
#[derive(Debug, Default, Clone)]
pub struct RuleSet {
    /// Tools allowed without asking for the rest of the session.
    pub always_allow_tools: Vec<String>,
    /// Tools refused outright for the rest of the session (blacklist).
    pub denied_tools: Vec<String>,
    /// Shell command prefixes allowed without asking. Narrow: `npm test`
    /// covers `npm test --watch` but never `npm publish`.
    pub shell_prefixes: Vec<String>,
}

/// Split a shell command into chain segments on unquoted `;`, `&`, `|`, `\n`.
/// Any of these breaks the chain — deliberately narrower than exact semantics.
pub fn parse_chain(cmd: &str) -> Vec<String> {
    let mut segs = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for ch in cmd.chars() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                }
                cur.push(ch);
            }
            None => match ch {
                '\'' | '"' => {
                    quote = Some(ch);
                    cur.push(ch);
                }
                ';' | '&' | '|' | '\n' => {
                    segs.push(std::mem::take(&mut cur));
                }
                _ => cur.push(ch),
            },
        }
    }
    segs.push(cur);
    segs.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

impl RuleSet {
    /// True when every chain segment matches an allowed prefix. Empty prefix
    /// list allows nothing.
    pub fn shell_allows(&self, cmd: &str) -> bool {
        let segs = parse_chain(cmd);
        if segs.is_empty() {
            return false;
        }
        segs.iter().all(|seg| {
            self.shell_prefixes.iter().any(|p| seg == p || seg.strip_prefix(p).is_some_and(|rest| rest.starts_with(' ')))
        })
    }
}

fn shell_command(args_json: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(args_json)
        .ok()?
        .get("command")?
        .as_str()
        .map(str::to_string)
}

/// Mutable permission state: mode + session rules + the approval gate.
pub struct PermissionState {
    mode: Mutex<PermissionMode>,
    rules: Mutex<RuleSet>,
    gate: Arc<dyn ApprovalGate>,
}

impl PermissionState {
    /// New state in `mode` with `gate` as the approval card sink.
    pub fn new(mode: PermissionMode, gate: Arc<dyn ApprovalGate>) -> Self {
        Self { mode: Mutex::new(mode), rules: Mutex::new(RuleSet::default()), gate }
    }

    /// Current mode.
    pub fn mode(&self) -> PermissionMode {
        *self.mode.lock().expect("mode lock")
    }

    /// Switch mode at runtime (e.g. `/goal` switches to autonomy).
    pub fn set_mode(&self, mode: PermissionMode) {
        *self.mode.lock().expect("mode lock") = mode;
    }

    /// Allow a tool for the rest of the session ("Always" on a card).
    pub fn allow_tool_always(&self, tool: &str) {
        self.rules.lock().expect("rules lock").always_allow_tools.push(tool.to_string());
    }

    /// Add a narrow shell prefix rule ("Always" on a shell card).
    pub fn allow_shell_prefix(&self, prefix: &str) {
        let p = prefix.trim().to_string();
        if !p.is_empty() {
            self.rules.lock().expect("rules lock").shell_prefixes.push(p);
        }
    }

    /// Deny a tool for the rest of the session.
    pub fn deny_tool(&self, tool: &str) {
        self.rules.lock().expect("rules lock").denied_tools.push(tool.to_string());
    }

    /// Decide for one call. `diff` is the write preview, computed by the caller.
    pub fn decide(&self, call: &ToolCall, diff: Option<String>) -> Verdict {
        let rules = self.rules.lock().expect("rules lock");
        if rules.denied_tools.iter().any(|t| t == &call.name) {
            return Verdict::Deny("tool is on the session blacklist".into());
        }
        if rules.always_allow_tools.iter().any(|t| t == &call.name) {
            return Verdict::Allow;
        }
        let mode = self.mode();
        match Category::from_tool(&call.name) {
            Category::Read | Category::Net => Verdict::Allow,
            Category::Write => match mode {
                PermissionMode::Bypass | PermissionMode::AcceptEdits => Verdict::Allow,
                PermissionMode::Planning => {
                    Verdict::Deny("planning mode is read-only".into())
                }
                PermissionMode::Manual => Verdict::NeedApproval { diff },
            },
            Category::Shell => {
                let cmd = shell_command(&call.args_json);
                let allowed_by_rule = cmd.as_deref().is_some_and(|c| rules.shell_allows(c));
                if allowed_by_rule {
                    return Verdict::Allow;
                }
                match mode {
                    PermissionMode::Bypass => Verdict::Allow,
                    PermissionMode::Planning => {
                        Verdict::Deny("shell command is not on the allow list in planning mode".into())
                    }
                    PermissionMode::Manual | PermissionMode::AcceptEdits => Verdict::NeedApproval { diff: None },
                }
            }
            Category::Mcp => match mode {
                PermissionMode::Bypass => Verdict::Allow,
                PermissionMode::Planning => {
                    Verdict::Deny("external tools are not available in planning mode".into())
                }
                PermissionMode::Manual | PermissionMode::AcceptEdits => Verdict::NeedApproval { diff: None },
            },
        }
    }

    /// The approval gate (so subagent wrappers can ask the same user).
    pub fn gate(&self) -> &Arc<dyn ApprovalGate> {
        &self.gate
    }
}

/// Wrap any [`ToolExec`] with the permission layer. The agent loop sees only
/// this object; the inner executor never runs without a green verdict.
pub struct PermissionedTools {
    inner: Arc<dyn ToolExec>,
    preview: Option<Arc<dyn crate::loop_::WritePreview>>,
    state: Arc<PermissionState>,
}

impl PermissionedTools {
    /// Wrap `inner`; `preview` supplies write diffs when the executor supports it.
    pub fn new(
        inner: Arc<dyn ToolExec>,
        preview: Option<Arc<dyn crate::loop_::WritePreview>>,
        state: Arc<PermissionState>,
    ) -> Self {
        Self { inner, preview, state }
    }

    /// Shared permission state (for the UI to switch modes and add rules).
    pub fn state(&self) -> &Arc<PermissionState> {
        &self.state
    }
}

#[async_trait]
impl ToolExec for PermissionedTools {
    fn specs(&self) -> Vec<ToolSpec> {
        self.inner.specs()
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let diff = self.preview.as_deref().and_then(|p| p.write_preview(call));
        match self.state.decide(call, diff) {
            Verdict::Allow => self.inner.execute(call).await,
            Verdict::Deny(reason) => ToolOutput {
                content: format!("denied by permissions: {reason}"),
                is_error: true,
            },
            Verdict::NeedApproval { diff } => {
                let req = ApprovalRequest {
                    tool: call.name.clone(),
                    args_json: call.args_json.clone(),
                    category: Category::from_tool(&call.name),
                    diff,
                };
                match self.state.gate.approve(&req).await {
                    Decision::Allow => self.inner.execute(call).await,
                    Decision::Deny => ToolOutput {
                        content: "denied by user".into(),
                        is_error: true,
                    },
                }
            }
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, args: &str) -> ToolCall {
        ToolCall { id: "t".into(), name: name.into(), args_json: args.into() }
    }

    #[test]
    fn chain_parsing_respects_quotes_and_splits_all_delimiters() {
        assert_eq!(parse_chain("npm test"), vec!["npm test".to_string()]);
        assert_eq!(
            parse_chain("a && b; c | d\ne"),
            vec!["a", "b", "c", "d", "e"].iter().map(|s| s.to_string()).collect::<Vec<_>>()
        );
        // Quoted delimiters do not split.
        assert_eq!(parse_chain("echo \"a && b\""), vec!["echo \"a && b\"".to_string()]);
        assert!(parse_chain("   ").is_empty());
    }

    #[test]
    fn narrow_rule_covers_extensions_not_redirects() {
        let mut rules = RuleSet::default();
        rules.shell_prefixes = vec!["npm test".into()];
        assert!(rules.shell_allows("npm test"));
        assert!(rules.shell_allows("npm test -- --watch"));
        assert!(!rules.shell_allows("npm publish"));
        // The killer case from the canon: allowed head + denied tail.
        assert!(!rules.shell_allows("npm test && npm publish"));
        // Prefix is not a raw string match: "npm testcase" must not pass.
        assert!(!rules.shell_allows("npm testcase"));
        assert!(!rules.shell_allows(""));
    }

    #[test]
    fn read_and_net_always_pass_all_modes() {
        for mode in [PermissionMode::Planning, PermissionMode::Manual, PermissionMode::AcceptEdits, PermissionMode::Bypass] {
            let state = PermissionState::new(mode, Arc::new(DenyAllGate));
            assert_eq!(state.decide(&call("read_file", r#"{"path":"a"}"#), None), Verdict::Allow);
            assert_eq!(state.decide(&call("grep", r#"{"pattern":"x"}"#), None), Verdict::Allow);
            assert_eq!(state.decide(&call("web_fetch", r#"{"url":"https://x"}"#), None), Verdict::Allow);
        }
    }

    #[test]
    fn write_follows_mode_table() {
        let args = r#"{"path":"a.txt","content":"hi"}"#;
        let state = PermissionState::new(PermissionMode::Manual, Arc::new(DenyAllGate));
        assert_eq!(
            state.decide(&call("write_file", args), Some("diff".into())),
            Verdict::NeedApproval { diff: Some("diff".into()) }
        );

        let state = PermissionState::new(PermissionMode::Planning, Arc::new(AllowAllGate));
        assert!(matches!(state.decide(&call("write_file", args), None), Verdict::Deny(_)));

        let state = PermissionState::new(PermissionMode::AcceptEdits, Arc::new(DenyAllGate));
        assert_eq!(state.decide(&call("write_file", args), None), Verdict::Allow);

        let state = PermissionState::new(PermissionMode::Bypass, Arc::new(DenyAllGate));
        assert_eq!(state.decide(&call("write_file", args), None), Verdict::Allow);
    }

    #[test]
    fn accept_edits_allows_writes_but_gates_shell() {
        let state = PermissionState::new(PermissionMode::AcceptEdits, Arc::new(DenyAllGate));
        let write = call("write_file", r#"{"path":"test.rs","content":"fn main() {}"}"#);
        assert_eq!(state.decide(&write, None), Verdict::Allow);

        let shell = call("run_shell", r#"{"command":"cargo build"}"#);
        assert!(matches!(state.decide(&shell, None), Verdict::NeedApproval { .. }));
    }

    #[test]
    fn shell_narrow_rules_beat_manual_approval() {
        let state = PermissionState::new(PermissionMode::Manual, Arc::new(DenyAllGate));
        let ok = call("run_shell", r#"{"command":"cargo test"}"#);
        let bad = call("run_shell", r#"{"command":"cargo publish"}"#);
        assert!(matches!(state.decide(&ok.clone(), None), Verdict::NeedApproval { .. }));
        state.allow_shell_prefix("cargo test");
        assert_eq!(state.decide(&ok, None), Verdict::Allow);
        assert!(matches!(state.decide(&bad, None), Verdict::NeedApproval { .. }));
    }

    #[test]
    fn planning_denies_unlisted_shell_but_allows_listed() {
        let state = PermissionState::new(PermissionMode::Planning, Arc::new(DenyAllGate));
        let ls = call("run_shell", r#"{"command":"ls -la"}"#);
        assert!(matches!(state.decide(&ls, None), Verdict::Deny(_)));
        state.allow_shell_prefix("ls");
        assert_eq!(state.decide(&call("run_shell", r#"{"command":"ls"}"#), None), Verdict::Allow);
    }

    #[test]
    fn blacklist_beats_everything() {
        let state = PermissionState::new(PermissionMode::Bypass, Arc::new(DenyAllGate));
        state.deny_tool("rm_tool");
        assert!(matches!(state.decide(&call("rm_tool", "{}"), None), Verdict::Deny(_)));
    }

    #[test]
    fn always_allow_beats_manual_mode() {
        let state = PermissionState::new(PermissionMode::Manual, Arc::new(DenyAllGate));
        state.allow_tool_always("write_file");
        assert_eq!(
            state.decide(&call("write_file", r#"{"path":"a"}"#), None),
            Verdict::Allow
        );
    }

    #[tokio::test]
    async fn wrapper_runs_inner_only_after_gate_allows() {
        use flashagent_llm::ToolSpec;

        struct Counting {
            runs: Mutex<u32>,
        }
        #[async_trait]
        impl ToolExec for Counting {
            async fn execute(&self, _call: &ToolCall) -> ToolOutput {
                *self.runs.lock().expect("runs lock") += 1;
                ToolOutput { content: "ran".into(), is_error: false }
            }
            fn specs(&self) -> Vec<ToolSpec> {
                vec![]
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
        }
        // Keep the concrete `Arc<Counting>` to read the counter; the wrapper
        // only ever sees it as `Arc<dyn ToolExec>`.
        let inner: Arc<Counting> = Arc::new(Counting { runs: Mutex::new(0) });
        let state = Arc::new(PermissionState::new(PermissionMode::Manual, Arc::new(DenyAllGate)));
        let inner_arc: Arc<dyn ToolExec> = inner.clone();
        let wrapped = PermissionedTools::new(inner_arc, None::<Arc<dyn crate::loop_::WritePreview>>, state);

        // Write denied by gate → inner never runs.
        let out = wrapped.execute(&call("write_file", r#"{"path":"a"}"#)).await;
        assert!(out.is_error && out.content.contains("denied by user"));
        assert_eq!(*inner.runs.lock().unwrap(), 0);

        // Read passes without the gate.
        let out = wrapped.execute(&call("read_file", r#"{"path":"a"}"#)).await;
        assert!(!out.is_error);
        assert_eq!(*inner.runs.lock().unwrap(), 1);

        // Switch the gate to AllowAll → write runs.
        let state2 = Arc::new(PermissionState::new(PermissionMode::Manual, Arc::new(AllowAllGate)));
        let inner_arc2: Arc<dyn ToolExec> = inner.clone();
        let wrapped = PermissionedTools::new(inner_arc2, None::<Arc<dyn crate::loop_::WritePreview>>, state2);
        let out = wrapped.execute(&call("write_file", r#"{"path":"a"}"#)).await;
        assert!(!out.is_error);
        assert_eq!(*inner.runs.lock().unwrap(), 2);
    }

    #[tokio::test]
    async fn denied_tool_short_circuits_before_gate() {
        struct Exploding;
        #[async_trait]
        impl ToolExec for Exploding {
            async fn execute(&self, _call: &ToolCall) -> ToolOutput {
                panic!("must not be called");
            }
            fn specs(&self) -> Vec<ToolSpec> {
                vec![]
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
        }
        let inner = Exploding;
        let state = Arc::new(PermissionState::new(PermissionMode::Bypass, Arc::new(DenyAllGate)));
        state.deny_tool("danger");
        let wrapped = PermissionedTools::new(
            Arc::new(inner),
            None::<Arc<dyn crate::loop_::WritePreview>>,
            state,
        );
        let out = wrapped.execute(&call("danger", "{}")).await;
        assert!(out.is_error && out.content.contains("blacklist"));
    }

    #[test]
    fn permission_mode_cycle_next_prev() {
        let m = PermissionMode::Planning;
        assert_eq!(m.next(), PermissionMode::Manual);
        assert_eq!(m.next().next(), PermissionMode::AcceptEdits);
        assert_eq!(m.next().next().next(), PermissionMode::Bypass);
        assert_eq!(m.next().next().next().next(), PermissionMode::Planning);

        assert_eq!(m.prev(), PermissionMode::Bypass);
        assert_eq!(m.prev().prev(), PermissionMode::AcceptEdits);
    }

    #[test]
    fn test_new_tools_categories_and_ask_user() {
        assert_eq!(Category::from_tool("ask_user"), Category::Read);
        assert_eq!(Category::from_tool("outline_file"), Category::Read);
        assert_eq!(Category::from_tool("git_status"), Category::Read);
        assert_eq!(Category::from_tool("git_diff"), Category::Read);
        assert_eq!(Category::from_tool("env_info"), Category::Read);
        assert_eq!(Category::from_tool("memory_read"), Category::Read);
        assert_eq!(Category::from_tool("patch_file"), Category::Write);
        assert_eq!(Category::from_tool("memory_create"), Category::Write);
        assert_eq!(Category::from_tool("memory_update"), Category::Write);
        assert_eq!(Category::from_tool("memory_remove"), Category::Write);

        let state_planning = PermissionState::new(PermissionMode::Planning, Arc::new(DenyAllGate));
        assert_eq!(state_planning.decide(&call("ask_user", "{}"), None), Verdict::Allow);
        assert_eq!(state_planning.decide(&call("outline_file", "{}"), None), Verdict::Allow);
        assert_eq!(
            state_planning.decide(&call("patch_file", "{}"), None),
            Verdict::Deny("planning mode is read-only".into())
        );

        let state_accept_edits = PermissionState::new(PermissionMode::AcceptEdits, Arc::new(DenyAllGate));
        assert_eq!(state_accept_edits.decide(&call("patch_file", "{}"), None), Verdict::Allow);
        assert_eq!(state_accept_edits.decide(&call("memory_create", "{}"), None), Verdict::Allow);
    }
}
