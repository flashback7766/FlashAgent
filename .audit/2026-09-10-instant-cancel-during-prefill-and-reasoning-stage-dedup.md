# Session: 2026-09-10 Instant Cancel During Prompt Prefill & Reasoning Stage Deduplication

### Status: PASS

### Decision:
Fixed two critical runtime issues reported with user screenshots:
1. **Instant cancellation during prompt prefill / generation (`Esc` key)**:
   - Root cause: When local LLMs (e.g. LM Studio with 35B model and large context) process prompt prefill, the client awaits `llm.turn_with_options` or `stream.next()` over HTTP for hundreds of seconds (377s in user screenshot). Previously, pressing `Esc` only flipped `cancel.store(true)` in memory and did not abort the awaiting tokio task or interrupt the network socket. Furthermore, `running = false` was only set upon receiving `UiEvent::Finished`, causing `Esc` to print `[Request interrupted by user]` dozens of times without stopping generation or clearing composer state (`Working on task...`).
   - Fix:
     - In `crates/core/src/loop_.rs`: Wrapped `llm.turn_with_options`, `stream.next()`, and `tools.execute` inside `tokio::select!` against a `wait_cancel` watcher. Any `cancel` trigger drops the network future within 25ms, cleanly closing the HTTP socket and immediately yielding `DoneReason::Cancelled`.
     - In `crates/tui/src/main.rs`: Retained `active_turn_handle: Option<tokio::task::JoinHandle<()>>` from `spawn_turn`. On `KeyCode::Esc`, immediately calls `handle.abort()`, sets `running = false`, resets `turn_started = None`, emits `LoopEvent::Done(DoneReason::Cancelled)` to chat, and requests instant reprint. Control returns to the user immediately.
2. **Reasoning header repetition in multi-step turns**:
   - Root cause: `reasoning_stage` checked `if lower.contains("user wants") || lower.contains("user asked")` ahead of specific action/tool results. Local models regularly open thoughts with "The user wants...", so intermediate reasoning steps after file searches or tool executions repeatedly received `Thought: Understanding user request`.
   - Fix:
     - In `crates/tui/src/lib.rs`: Added `resolve_reasoning_stage` and `derive_stage_from_tool`. It checks turn history: if tools have already executed (`tools_executed > 0`), "initial understanding" headers are overridden by the actual tool context (e.g. `Evaluating Search Results` after grep/glob, `Analyzing File Contents` after reading files). Furthermore, any candidate identical to a prior stage in the same turn is advanced along a natural progression (`Synthesizing Findings`, `Planning Implementation`, etc.), guaranteeing 100% unique headers.
     - In `crates/core/src/prompt.rs`: Added explicit STAGE PROGRESSION guidelines instructing the model to advance forward and strictly prohibiting reusing earlier stage headers like `**Understanding the Request**` after tool calls.

### Files touched:
- `crates/core/src/loop_.rs`
- `crates/core/src/prompt.rs`
- `crates/tui/src/lib.rs`
- `crates/tui/src/main.rs`

### Verification:
```bash
cargo test --workspace
# Output: 173 passed; 0 failed; finished in 0.35s / 0.01s / 1.03s. Exit code: 0

cargo clippy --workspace -- -D warnings
# Output: Finished dev profile, 0 warnings. Exit code: 0

cargo build --release
# Output: Finished release profile [optimized] target(s) in 12.63s. Exit code: 0

cargo test -p flashagent-ui
# Output: 5 passed; 0 failed. Exit code: 0
```

### Open questions:
None.

### Handoff:
Prompt prefill cancellation now aborts immediately on Esc and returns the composer to an active input state. Reasoning stage headers adapt to tool execution history and remain unique across each turn.
