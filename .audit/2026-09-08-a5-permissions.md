# 2026-09-08 — A5: Permissions & Approval Gate

### Status: PASS
### Decision: A5 (permissions layer in crates/core)

Files touched:
- crates/core/src/permissions.rs (new: PermissionMode, ToolCategory, Request, Decision, ApprovalGate, State, PermissionedTools)
- crates/core/src/lib.rs (exports)
- crates/tools/src/fs_tools.rs (write_preview: unified diff for write and edit tools)

Verification:
- `cargo test --workspace` → TOTAL passed: 69, failed: 0 (core: 26/26, tools: 13/13, llm: 20/20, data: 5/5, ui: 5/5)
- `cargo clippy --workspace -- -D warnings` → Finished, 0 warnings

Implemented capabilities:
- PermissionMode: Manual (all write operations ask), Autonomic (shell asks, filesystem approved), Planning (read-only, write/shell blocked), Bypass (all allowed per prompt)
- ToolCategory: ReadOnly, FileWrite, Shell, Web, Mcp
- ApprovalGate: async channel-based gate between agent loop and user interface
- Decisions: AllowOnce, AllowAlways (persists in session per tool), Deny
- write_preview: unified diff generator for write_file and edit_file before execution
- Session persistence: allow_tool_always persists across turns within active session

Open questions:
- None.

Handoff:
- Next: A6 — memory layer (MEMORY.md, outline, session summaries, dynamic context injection).
