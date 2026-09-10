# Audit: Real-Time Model Context Discovery & Esc Auto-Cancel Bugfix

### Status: PASS
Decision: TUI-A7.5 (Real-time server discovery on context reload without restart, atomic cancel flag lifecycle, suggested prompt clearing on Esc / cancellation)

### Files touched:
- `crates/llm/src/thinking.rs`
- `crates/llm/src/openai.rs`
- `crates/tui/src/main.rs`

### Summary of Fixes:

1. **Esc Auto-Cancellation & Ghost Prompt Freeze Bug (Critical Fix)**:
   - **Root Cause**:
     - When pressing `Esc` during a turn, `cancel.store(true, Ordering::Relaxed)` was set. However, `cancel` was never reset to `false` when the turn finished or when a new turn started. Every subsequent turn immediately detected `cancel == true` at step 0 and returned `DoneReason::Cancelled`, causing all user messages to be cancelled instantly with `— Cancelled —`.
     - In `UiEvent::Finished`, `Ok((h, _))` ignored `reason` and unconditionally ran `build_turn_recap_and_suggestion(&history)`. Since `history` on cancelled turns had no assistant reply, it picked up the previous turn's exploration step and permanently re-set `suggested_prompt = Some("What's next?")` in the composer (`What's next? (→ to use)`).
   - **Resolution**:
     - `cancel.store(false, Ordering::Relaxed)` is now guaranteed upon `UiEvent::Finished`, before launching any new turn in `spawn_turn`, and upon prompt submission (`/goal`, `/skill:`, and normal prompt in `KeyCode::Enter`).
     - In `UiEvent::Finished`: when `reason == DoneReason::Cancelled`, clear `suggested_prompt = None` and `custom_placeholder = None`, and skip generating turn recaps and background ghost suggestions.
     - In `UiEvent::BackgroundRecap`: verify `turn_id == current_turn_id` before updating suggestions, preventing stale background suggestions from overwriting current state.
     - In `KeyCode::Esc`: clear `suggested_prompt = None` and `custom_placeholder = None` when interrupted or when dismissing suggestions. If the user has typed text, `Esc` clears the draft instead of abruptly terminating the application.

2. **Real-Time Context Window Discovery (64k -> 128k Reload Without Restart)**:
   - **Root Cause**:
     - Periodic discovery previously checked only `if active.id != current_model`. When reloading the same model in LM Studio with a larger context window (64k -> 128k), `active.id` was identical, so context updates were ignored.
     - Context capacity `context_usage.total_capacity` was only initialized at startup and never updated on discovery.
     - In `crates/llm/src/openai.rs`, `discover_server()` preferred any model matching `current_model` over actively loaded models (`is_loaded`), potentially keeping stale or unloaded configurations.
     - Discovery was running synchronously on the UI event loop every interval, risking UI stutter during server reloads.
   - **Resolution**:
     - `crates/llm/src/openai.rs`: Prioritized actively loaded models (`m.is_loaded`) in server memory during discovery, correctly capturing reloads and active context lengths.
     - `crates/llm/src/thinking.rs`: Added explicit mapping for `65_536` / `64_000` to `"64k ctx"` and `131_072` / `128_000` to `"128k ctx"`.
     - `crates/tui/src/main.rs`: Offloaded periodic server discovery to a non-blocking background tokio task emitting `UiEvent::ServerDiscovered`.
     - Handled `UiEvent::ServerDiscovered`: checks both `model_changed` and `ctx_changed` (`context_usage.total_capacity != new_ctx_len || current_context != new_ctx_disp`). Updates `current_context`, `context_usage.total_capacity`, recalculates token gauge percentages, refreshes welcome card (mascot info), and emits a system notification informing the user of the context capacity update in real time.

### Verification:

```text
$ cargo test --workspace
running 25 tests in flashagent_llm ... ok (25 passed)
running 30 tests in flashagent_tools ... ok (30 passed)
running 51 tests in flashagent_tui ... ok (51 passed)
running 5 tests in flashagent_ui ... ok (5 passed)
test result: ok. 111 passed; 0 failed; 0 ignored; finished in 1.37s
Exit code: 0

$ cargo clippy --workspace -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.00s
Exit code: 0

$ cargo build --release
Finished `release` profile [optimized] target(s) in 12.86s
Exit code: 0

$ ./flashagent --help
FlashAgent TUI
Usage: flashagent-tui [OPTIONS]
Exit code: 0
```

### Open questions:
None.

### Handoff:
- Release executable is built at `target/release/flashagent-tui` and linked at `./flashagent`.
- Local model reloads in LM Studio/Ollama now dynamically update context capacity (64k -> 128k) in real time.
- Pressing Esc properly cancels only the active turn; subsequent prompts execute smoothly without auto-cancelling or freezing ghost suggestions.
