# Audit: Prefill Tracker & TTFT, Non-blocking Tabs, Mid-Flight Steering (Option B)

### Status: PASS
Decision: Option B Mid-Flight Steering + Model Prefill Tracker/TTFT + Non-blocking Tabs/Menus + LM Studio Binary Reasoning Guard
Files touched:
- crates/llm/src/thinking.rs
- crates/llm/src/openai.rs
- crates/core/src/loop_.rs
- crates/tui/Cargo.toml
- crates/tui/src/lib.rs
- crates/tui/src/prefill.rs
- crates/tui/src/main.rs

### Summary of Changes:
1. **LM Studio Warning Elimination**:
   - In `crates/llm/src/thinking.rs` and `crates/llm/src/openai.rs`, added detection for binary on/off models (`is_binary_on_off`) in `ThinkingProtocol::LmStudio`.
   - Suppressed sending unsupported `reasoning_effort` strings ("medium", "high", etc.) to LM Studio models that only support binary on/off thinking toggles. Eliminates LM Studio warning: `[WARN] Reasoning setting 'medium' is not supported by the model...`.

2. **Per-Model Prefill Evaluation Speed & TTFT Prediction Engine**:
   - Created `crates/tui/src/prefill.rs` with `PrefillTracker`, `ModelPrefillProfile`, and `ContextBucket` (`<2k`, `2k-8k`, `8k-32k`, `32k+`).
   - Tracks actual TTFT (time-to-first-token), calculates prefill processing rate (`tokens / sec`) based on prompt tokens, and maintains exponential moving averages (EMA) per model and bucket.
   - Persists learned profiles across sessions to `~/.flashagent/prefill_cache.json`.
   - Real-time prefill visual feedback in the composer placeholder during prompt evaluation: `⚡ Prefill ~1.4s (3.2k tok @ 2.4k t/s) [••••••  ]`.
   - Exact TTFT displayed in turn stats and persistent footer once streaming begins: `⚡ TTFT 1.35s (2.4k t/s prefill)`.

3. **Seamless Non-blocking Tabs / Menus During Generation**:
   - Removed `!running` gatekeepers from `Tab` (Settings tab), `F3` (Model selection menu), `F4` (Thinking effort menu), and `F5` (Sampling parameters menu).
   - The composer area morphs into the requested menu while the chat stream and reasoning above continue streaming tokens live and uninterrupted.
   - Keyboard navigation within open menus intercepts `Esc` and closes only the menu, never aborting the active generation. Only pressing `Esc` without any active menu cancels generation.

4. **Mid-Flight Steering Engine (Approved Option B)**:
   - Added `LoopEvent::SteeringInjected(String)` and `steer_rx` channel in `AgentLoop::with_steering` (`crates/core/src/loop_.rs`).
   - Hybrid Option B implementation:
     - Mid-stream (text / reasoning): incoming steering prompt immediately interrupts the stream, commits the partial assistant message with clean empty `tool_calls`, records `[STEERING DIRECTIVE]`, and triggers an immediate turn restart (~200ms turnaround).
     - Tool-execution phase: preserves strict OpenAI API invariants (`tool_calls` must be answered by `tool` result before any user message). Tools run to completion and record results; steering directives queued in `steer_rx` are then injected cleanly at the step boundary.
   - Wires `active_steer_tx` to `KeyCode::Enter` when typing while `running`: user types instructions and hits `Enter` to steer the model mid-flight.

### Verification:
```bash
cargo test --workspace
```
Exit code: 0
Output snippet:
- `flashagent-core`: 51 passed; 0 failed (includes `test_steering_mid_stream_interrupts_and_pivots` and `test_steering_during_tool_execution_preserves_protocol`)
- `flashagent-llm`: 17 passed; 0 failed
- `flashagent-tools`: 36 passed; 0 failed
- `flashagent-tui`: 71 lib tests passed, 5 bin tests passed; 0 failed (includes `test_context_buckets_classification`, `test_model_prefill_profile_recording_and_prediction`, `test_prefill_tracker_live_and_completed_formatting`)
- Total tests passed: 185, 0 failed.

```bash
cargo clippy --workspace -- -D warnings
```
Exit code: 0 (Zero warnings, clean workspace)

```bash
cargo build --release
```
Exit code: 0 (Binary built at target/release/flashagent-tui, verified symlinked at ./flashagent)

Open questions: None
Handoff: Ready for user testing.
