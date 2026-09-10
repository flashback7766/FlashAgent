# Audit: 2026-09-09 TUI Silent Reasoning Toggle Fix

### Status: PASS
Decision: C5 (TUI reasoning stream integrity and silent toggle)

Files touched:
- `crates/tui/src/main.rs`:
  - Removed pushing `[Reasoning view: ...]` system messages into the chat scrollback on `F2`, `Alt+O`, `Ctrl+O`, and slash `/expand` commands.
  - The status is already clearly shown in the input box status line (`[reasoning: all]`, `[reasoning: last]`).
  - Switching expansion mode now cleanly repaints without inserting intermediate system messages into `ChatView.lines` or resetting `streaming_reasoning`.
- `crates/tui/src/lib.rs`:
  - Fixed `ChatView::update_or_push_system`: updating an existing system message in place now preserves active `streaming` and `streaming_reasoning` pointers instead of resetting them to `None`.
  - Added unit test `update_or_push_system_does_not_break_active_streaming_reasoning`.

Verification:
- `cargo test --workspace` -> Exit code 0 (109 passed; 0 failed)
- `cargo clippy --workspace -- -D warnings` -> Exit code 0 (0 warnings)

Open questions:
- None. Toggling reasoning view is now completely silent and never interrupts or fragments the reasoning stream.

Handoff:
- The reasoning container remains one unified block when toggled.
- Ready to proceed with Milestone A9 (MCP implementation plan).
