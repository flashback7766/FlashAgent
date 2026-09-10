# 2026-09-08 — A8: Subagents (Dynamic Roles, Channels, Parent Review, Permission Inheritance)

### Status: PASS
### Decision: Subagents integrated as `spawn_agent` tool; logic in `core::subagents`; mpsc channels per id; parent review via Role::Tool result; permissions inherited, never expanded (PHILOSOPHY §6-7).

Files touched:
- crates/core/src/subagents.rs (NEW):
  - AgentRole { name, system_prompt, tools } — dynamic role
  - SubagentSpec { role, prompt, max_steps, timeout, max_output_chars }
  - SubagentMsg { Text, Finished, Cancel } — typed mpsc messages by id
  - SubagentHandle { id, rx, task }
  - SubagentHost::new/spawn/live — orchestrator (live counter, mpsc channels)
  - SubagentTool — spawn_agent tool (result = Role::Tool, preventing injections)
  - SubagentToolFactory — trait for constructing child ToolExec
  - role_prompt — system prompts for roles (researcher/coder/reviewer/planner)
  - 5 contract tests (spawn+reply, timeout, role, tool result, live counter)
- crates/core/src/permissions.rs:
  - PermissionedTools transitioned to owning `Arc<dyn ToolExec>`
  - Added PermissionState::gate() (gate access for subagent)
- crates/core/src/loop_.rs:
  - ToolExec::as_any() — downcast for tests/adapters
- crates/core/src/lib.rs: pub mod subagents + exports
- crates/tools/src/subagents.rs (NEW):
  - ToolSubset — tool filtering by name (filters specs, blocks execution)
  - BuiltinSubagentFactory — PermissionedTools on top of BuiltinTools with shared state
  - CompositeTools + agent_tools — combines BuiltinTools + spawn_agent
  - 2 tests (subset hides/blocks, empty subset allows all)
- crates/tools/src/lib.rs: export subagents
- crates/tui/src/main.rs:
  - source → Arc<BackendSource>; spawn_turn accepts Arc
  - perm = PermissionedTools(agent_tools(...)) — parent sees spawn_agent

Verification:
- cargo test --workspace → 92 passed, 0 failed (core 33: +5 subagents; tools 15: +2 subset)
- cargo clippy --workspace -- -D warnings → 0 warnings

Architectural decisions:
- Subagent = tool. Parent loop invokes spawn_agent, executing separate child AgentLoop with restricted tool subset.
- Channels: each subagent has its own mpsc receiver addressed by unique ID.
- Permission inheritance: subagent inherits parent PermissionState and never elevates permissions.

Handoff:
- Next: A9 — MCP integration or UI enhancement.
